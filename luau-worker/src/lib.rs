use futures_util::StreamExt;
extern crate console_error_panic_hook;

use base64::prelude::*;
use luau_lifter::decompile_bytecode_with_script_name;
use serde::{Deserialize, Serialize};
use worker::*;

const AUTH_SECRET: &str = "ymjKH2O3BbO3bDSsKmpo3ek3vHxIWYLQfj0";

#[derive(Deserialize)]
struct DecompileMessage {
    id: String,
    encoded_bytecode: String,
    #[serde(default, alias = "scriptName")]
    script_name: Option<String>,
}

#[derive(Serialize)]
struct DecompileResponse {
    id: String,
    decompilation: String,
}

#[event(fetch, respond_with_errors)]
pub async fn main(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();
    router
        .get_async("/decompile_ws", |req, _ctx| async move {
            let license = req
                .headers()
                .get("Authorization")
                .unwrap_or_default()
                .expect("authorization header is required");

            if license != AUTH_SECRET {
                return Response::error("invalid license", 403);
            }

            let pair = WebSocketPair::new()?;
            let server = pair.server;
            server.accept()?;

            wasm_bindgen_futures::spawn_local(async move {
                let mut event_stream = server.events().expect("could not open stream");
                while let Some(event) = event_stream.next().await {
                    if let WebsocketEvent::Message(msg) =
                        event.expect("received error in websocket")
                    {
                        let msg = msg
                            .json::<DecompileMessage>()
                            .expect("malformed decompile message");
                        let bytecode = BASE64_STANDARD
                            .decode(msg.encoded_bytecode)
                            .expect("bytecode must be base64 encoded");
                        let resp = DecompileResponse {
                            id: msg.id,
                            decompilation: decompile_bytecode_with_script_name(
                                &bytecode,
                                1,
                                msg.script_name.as_deref(),
                            ),
                        };
                        server
                            .send_with_str(serde_json::to_string(&resp).unwrap())
                            .unwrap();
                    }
                }
            });

            Response::from_websocket(pair.client)
        })
        .post_async("/decompile", |mut req, _ctx| async move {
            let license = req
                .headers()
                .get("Authorization")
                .unwrap_or_default()
                .expect("authorization header is required");

            if license != AUTH_SECRET {
                return Response::error("invalid license", 403);
            }

            let script_name = req.headers().get("X-Script-Name").ok().flatten();
            let encoded_bytecode = req.bytes().await?;
            match BASE64_STANDARD.decode(encoded_bytecode) {
                Ok(bytecode) => Response::ok(decompile_bytecode_with_script_name(
                    &bytecode,
                    203,
                    script_name.as_deref(),
                )),
                Err(_) => Response::error("invalid bytecode", 400),
            }
        })
        .run(req, env)
        .await
}
