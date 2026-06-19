use itertools::Either;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    inline_temps::{collect_usage, Usage},
    Binary, BinaryOperation, Block, Call, Index, Literal, Local, LocalRw, MethodCall, RValue,
    RcLocal, Select, Statement, Table, Traverse,
};

// Lua syntactic keywords. A generated name must never be one of these.
const RESERVED_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// Stable identity of a local, based on the address of its backing allocation.
/// Using the address (instead of cloning the `Arc`) avoids inflating the
/// strong count, which `name_one` relies on to detect unused locals.
fn local_ptr(local: &RcLocal) -> usize {
    &*local.0 .0 as *const _ as usize
}

#[derive(Clone, Copy)]
enum IdentifierCase {
    LowerCamel,
    Pascal,
    Preserve,
}

#[derive(Clone)]
struct Hint {
    name: String,
    score: u8,
}

/// Turn an arbitrary hint string (a field name, service name, type name, ...)
/// into a valid local identifier, or `None` if it can't be used.
fn sanitize_with_case(raw: &str, case: IdentifierCase) -> Option<String> {
    let mut chars: Vec<char> = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if chars.is_empty() {
        return None;
    }
    match case {
        IdentifierCase::LowerCamel if chars[0].is_ascii_uppercase() => {
            chars[0] = chars[0].to_ascii_lowercase();
        }
        IdentifierCase::Pascal if chars[0].is_ascii_lowercase() => {
            chars[0] = chars[0].to_ascii_uppercase();
        }
        _ => {}
    }
    // identifiers can't start with a digit
    if chars[0].is_ascii_digit() {
        chars.insert(0, '_');
    }
    chars.truncate(32);
    let name: String = chars.into_iter().collect();
    // `self` is rejected here (it is not a Lua keyword, so it is not in
    // RESERVED_KEYWORDS): a local accidentally named `self` (e.g. an
    // index/field hint on `t.self`, or a global named `self`) would trip
    // recover_methods' `block_mentions_self_name` guard and the formatter's
    // colon-method detection, silently suppressing legitimate `T:method()`
    // recovery (§2.8). `self` is only ever produced deliberately by
    // recover_methods, never by name inference.
    if name == "_" || name == "self" || RESERVED_KEYWORDS.contains(&name.as_str()) {
        return None;
    }
    Some(name)
}

/// Most locals are still lowerCamelCase.
fn sanitize(raw: &str) -> Option<String> {
    sanitize_with_case(raw, IdentifierCase::LowerCamel)
}

/// Roblox service/module locals in source are commonly PascalCase.
fn sanitize_pascal(raw: &str) -> Option<String> {
    sanitize_with_case(raw, IdentifierCase::Pascal)
}

fn sanitize_preserve(raw: &str) -> Option<String> {
    sanitize_with_case(raw, IdentifierCase::Preserve)
}

/// A name derived from a "base" expression, e.g. the `Instance` in `Instance.new`
/// or the global in `require(...)`.
fn base_name_of(rvalue: &RValue) -> Option<String> {
    match rvalue {
        RValue::Global(global) => std::str::from_utf8(&global.0).ok().and_then(sanitize),
        RValue::Index(index) => index_hint(index),
        // require(script.Parent:WaitForChild("Notification")) -> "notification":
        // the module name lives in the trailing :WaitForChild/:FindFirstChild arg.
        RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
            method_call_hint(method_call)
        }
        _ => None,
    }
}

fn index_hint(index: &Index) -> Option<String> {
    if let RValue::Literal(Literal::String(key)) = &*index.right {
        return std::str::from_utf8(key).ok().and_then(sanitize);
    }
    None
}

fn call_hint(call: &Call) -> Option<String> {
    // require(script.Foo) -> "foo"
    if let RValue::Global(global) = &*call.value
        && global.0.as_slice() == b"require"
        && let Some(arg) = call.arguments.first()
        && let Some(name) = base_name_of(arg)
    {
        return Some(name);
    }
    // A numeric/string coercion is transparent for naming: the wrapped value
    // names the local. `tonumber(afkConfig.PlaceId)` -> "placeId",
    // `tostring(inst:GetAttribute("OwnerId"))` -> "ownerId". Only a single
    // argument is recursed (a 2-arg `tonumber(v, 16)` base-conversion carries no
    // name and is left alone); a bare local/literal arg yields None. The local's
    // OWN RHS is a Call (this `tonumber(..)`), which is non-movable, so naming the
    // result is sound regardless of what the inner hint resolves to (it may be a
    // field/method name, or a Global name for `tonumber(SomeGlobal)`).
    if let RValue::Global(global) = &*call.value
        && matches!(global.0.as_slice(), b"tonumber" | b"tostring")
        && call.arguments.len() == 1
    {
        return rvalue_hint(&call.arguments[0]);
    }
    // Bare time reads name the local after the captured clock value:
    // `local timestamp = os.clock()` / `tick()`. Requires ZERO arguments. We use
    // `timestamp` rather than `now`: such a value is very often STORED and read
    // later as a subtraction base (`os.clock() - timestamp`), where `now` would be
    // misleading (it is a past/start time by then) but `timestamp` stays accurate.
    // The offset form (`os.clock() + delay`) is a `Binary` RHS that never reaches
    // `call_hint`, so it correctly stays unnamed (source names those
    // `deadline`/`elapsed`).
    if call.arguments.is_empty() {
        let is_time = match &*call.value {
            RValue::Global(g) => g.0.as_slice() == b"tick",
            RValue::Index(index) => {
                global_name(&index.left) == Some("os")
                    && matches!(index_key(index), Some("clock") | Some("time"))
            }
            _ => false,
        };
        if is_time {
            return Some("timestamp".to_string());
        }
    }
    // Constructor-style calls read as their type:
    //   Instance.new("Part") -> "part" ; Color3.new(...) / Color3.fromRGB(...) -> "color"
    if let RValue::Index(index) = &*call.value
        && let RValue::Literal(Literal::String(method)) = &*index.right
    {
        let method = method.as_slice();
        if method == b"new" {
            if let Some(RValue::Literal(Literal::String(arg))) = call.arguments.first()
                && let Some(name) = std::str::from_utf8(arg).ok().and_then(sanitize)
            {
                return Some(name);
            }
            return constructor_type_name(&index.left);
        }
        // Alternate constructors (`Color3.fromRGB`, `Vector3.fromAxis`, ...) name
        // the local after the constructor type exactly as `.new` does. Their
        // arguments are scalars, so only the type-name fallback applies.
        if matches!(method, b"fromRGB" | b"fromHSV" | b"fromHex" | b"fromName" | b"fromAxis") {
            return constructor_type_name(&index.left);
        }
    }
    None
}

/// The variable name for a `Type.new(...)` / `Type.fromRGB(...)` constructor,
/// derived from the receiver type. A type whose name ends in a digit (`Color3`,
/// `Vector3`, `Vector2`, `UDim2`, `Region3`) is stripped of the trailing digits:
/// the digit makes a misleading disambiguating suffix — `color3` then collides to
/// `color32`, which reads as "color thirty-two" rather than "the 3rd color". The
/// trimmed form chains cleanly as `color`, `color2`, `color3`. `Instance` and
/// other digit-free types are unaffected.
fn constructor_type_name(receiver: &RValue) -> Option<String> {
    let base = base_name_of(receiver)?;
    let trimmed = base.trim_end_matches(|c: char| c.is_ascii_digit());
    Some(if trimmed.is_empty() {
        base
    } else {
        trimmed.to_string()
    })
}

/// The descriptive stem of a boolean-predicate function name: `isGraphicsDisabled`
/// -> `GraphicsDisabled`, `hasOwner` -> `Owner`. Returns `None` unless the name
/// starts with a recognised predicate verb (`is`/`has`) *immediately followed by an
/// uppercase letter*, so non-predicates like `island`/`hasher`/`issue` and the bare
/// verbs `is`/`has` are left alone. The remainder is returned with its original case
/// for the caller to `sanitize` into lowerCamel (§2.7 Layer A). Only `is`/`has` are
/// recognised: they strip to a noun/adjective that reads as a boolean
/// (`graphicsDisabled`), whereas `can`/`should`/`will` would strip to an imperative
/// verb (`canEdit` -> `edit`) that reads worse than the default `v`, and have zero
/// corpus sites anyway.
fn strip_predicate_prefix(name: &str) -> Option<&str> {
    const PREDICATE_PREFIXES: &[&str] = &["is", "has"];
    for prefix in PREDICATE_PREFIXES {
        if let Some(rest) = name.strip_prefix(prefix)
            && rest.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        {
            return Some(rest);
        }
    }
    None
}

fn method_call_hint(method_call: &MethodCall) -> Option<String> {
    let method = method_call.method.as_str();
    // Lookups carrying the name as a string argument:
    // obj:GetService("Players"), obj:FindFirstChild("Humanoid"), obj:WaitForChild("Remote")
    if method == "GetService"
        && let Some(RValue::Literal(Literal::String(arg))) = method_call.arguments.first()
    {
        return std::str::from_utf8(arg).ok().and_then(sanitize_pascal);
    }
    if (method.starts_with("FindFirst") || method.starts_with("WaitFor"))
        && let Some(RValue::Literal(Literal::String(arg))) = method_call.arguments.first()
    {
        return std::str::from_utf8(arg).ok().and_then(sanitize);
    }
    // The ATTRIBUTE KEY names the local (`inst:GetAttribute("OwnerId")` ->
    // "ownerId"), not the generic "attribute" the `Get`-prefix rule below would
    // otherwise yield. Must precede the `Get`-prefix strip. A dynamic-key
    // `:GetAttribute(var)` has no string literal, so it falls through to the
    // generic getter rule (-> "attribute"), unchanged.
    if method == "GetAttribute"
        && let Some(RValue::Literal(Literal::String(arg))) = method_call.arguments.first()
    {
        return std::str::from_utf8(arg).ok().and_then(sanitize);
    }
    // Result-of-method idioms with a fixed, near-universal source name. A stored
    // signal connection reads as `connection`, a played animation as `track`, a
    // cloned instance as `clone`. (Distinct from the existing event-callback
    // PARAM naming, which names the closure's arguments, not this result local.)
    match method {
        "Connect" | "Once" | "ConnectParallel" => return Some("connection".to_string()),
        "LoadAnimation" => return Some("track".to_string()),
        "Clone" => return Some("clone".to_string()),
        _ => {}
    }
    // Getter-style methods: obj:GetChildren() -> "children", obj:GetMouse() -> "mouse"
    if let Some(rest) = method.strip_prefix("Get")
        && !rest.is_empty()
    {
        return sanitize(rest);
    }
    None
}

/// The instance-lookup `MethodCall` at the heart of a nil-guarded lookup, looking
/// through both the `and` guard and a trailing `... or default`:
///   `X and X:FindFirstChild("Name")`              -> the `:FindFirstChild` call
///   `X and X:FindFirstChild("Name") or default`   -> same
///   `X:FindFirstChild("Name") or fallback`        -> same (bare primary)
///
/// Luau emits these short-circuit forms pervasively: the `and` only nil-guards
/// `X`, and the local stores the *lookup result*, so it deserves the same name a
/// bare `X:FindFirstChild("Name")` gets. `and` names from its right operand (the
/// guarded access); `or` names from its left operand (the primary), never the
/// fallback. Returns `None` for anything that isn't such a lookup. Both naming
/// layers route through this one function so they can never drift apart.
fn binary_lookup_method_call(binary: &Binary) -> Option<&MethodCall> {
    match binary.operation {
        BinaryOperation::And => match &*binary.right {
            RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
                Some(method_call)
            }
            _ => None,
        },
        BinaryOperation::Or => match &*binary.left {
            RValue::Binary(inner) => binary_lookup_method_call(inner),
            RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
                Some(method_call)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Name a local after the nil-guarded instance lookup it stores, exactly as the
/// bare call would be named (see [`binary_lookup_method_call`]).
fn binary_lookup_hint(binary: &Binary) -> Option<String> {
    method_call_hint(binary_lookup_method_call(binary)?)
}

/// The descriptive key of a boolean test's subject: a field access names after its
/// last key (`X.HideBaseParts` -> `HideBaseParts`), an attribute read after its
/// literal name (`X:GetAttribute("IsPlanted")` -> `IsPlanted`). Restricted to these
/// two shapes — a bare local / computed index `X[expr]` / arbitrary call carries no
/// reliable name.
fn boolean_subject_key(rvalue: &RValue) -> Option<&str> {
    match rvalue {
        RValue::Index(index) => index_key(index),
        RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call))
            if method_call.method == "GetAttribute" =>
        {
            method_call.arguments.first().and_then(string_literal)
        }
        _ => None,
    }
}

fn bool_literal(rvalue: &RValue) -> Option<bool> {
    if let RValue::Literal(Literal::Boolean(b)) = rvalue {
        Some(*b)
    } else {
        None
    }
}

/// Name a local bound to a boolean field/attribute test (§2.7 Layer B):
/// `local v = X.Field == true` -> `field`, `local v = inst:GetAttribute("Planted")
/// == true` -> `planted`. A leading `_` (private marker) is dropped so `_isOpen`
/// reads as `isOpen`; the attribute key is NOT stem-stripped, so `IsPlanted` ->
/// `isPlanted` matches source.
///
/// Two soundness constraints, both essential to avoid a misleading name:
/// - The other operand must be a *boolean literal*, so `X.Field ~= nil` is excluded
///   by construction — its value is a boolean, not the field (source calls
///   `Parent ~= nil` `hadParent`, never `parent`).
/// - Only *positive-polarity* tests are named — `X == true` and `X ~= false`, where
///   the result tracks the field's truthiness. A negated test (`X == false`,
///   `X ~= true`) yields the OPPOSITE boolean, so naming after the field misleads
///   (source calls `IsFavorite ~= true` `newState`, not `isFavorite`).
fn boolean_compare_hint(rvalue: &RValue) -> Option<String> {
    let RValue::Binary(binary) = rvalue else {
        return None;
    };
    // `positive` is true for `==` (result == field-truthiness) and false for `~=`.
    let positive = match binary.operation {
        BinaryOperation::Equal => true,
        BinaryOperation::NotEqual => false,
        _ => return None,
    };
    // Exactly one operand must be a boolean literal; name after the other. (Corpus
    // always has the literal on the right; the left case is handled defensively.)
    let (subject, literal) = match (bool_literal(&binary.left), bool_literal(&binary.right)) {
        (None, Some(b)) => (&*binary.left, b),
        (Some(b), None) => (&*binary.right, b),
        _ => return None,
    };
    // Name only when the result equals the field's truthiness: `== true` / `~= false`.
    if positive != literal {
        return None;
    }
    sanitize(boolean_subject_key(subject)?.trim_start_matches('_'))
}

/// Lookup child names that carry little information on their own and, when several
/// siblings look one up, collide into `client`/`client2`. These get qualified with
/// the receiver's name (`plantedSeeds` + `Client` -> `plantedSeedsClient`). Kept
/// small and structural on purpose: a specific child (`PlantedSeeds`, `Humanoid`)
/// is informative alone and must stay bare.
fn is_generic_lookup_child(name: &str) -> bool {
    matches!(
        name,
        "client"
            | "server"
            | "main"
            | "frame"
            | "container"
            | "holder"
            | "wrapper"
            | "object"
            | "model"
            | "folder"
            | "root"
            | "gui"
            | "ui"
    )
}

/// A generated default name (`v`, `p`, `v2`, `p3`, ...). A receiver named only by
/// such a default is no better than the bare child, so qualification is refused.
fn is_default_name(name: &str) -> bool {
    let stem = name.trim_end_matches(|c: char| c.is_ascii_digit());
    stem == "v" || stem == "p"
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

/// Whether `parent` already ends with `child` (case-insensitive), so qualifying
/// would only stutter — `clientModel` + `Model` -> `clientModelModel`, or the
/// degenerate `client` + `Client`. In that case the bare child name is kept.
fn name_ends_with_word(parent: &str, child: &str) -> bool {
    parent
        .to_ascii_lowercase()
        .ends_with(&child.to_ascii_lowercase())
}

/// Best-effort meaningful name for the value assigned to a local.
fn rvalue_hint(rvalue: &RValue) -> Option<String> {
    match rvalue {
        RValue::Index(index) => index_hint(index),
        RValue::Call(call) | RValue::Select(Select::Call(call)) => call_hint(call),
        RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
            method_call_hint(method_call)
        }
        RValue::Global(global) => std::str::from_utf8(&global.0).ok().and_then(sanitize),
        // `folder and folder:FindFirstChild("Client")` and friends: a nil-guarded
        // instance lookup is named after the lookup, exactly as the bare call is.
        RValue::Binary(binary) => binary_lookup_hint(binary),
        // Luau bytecode keeps a debug name for each function (e.g. the name a
        // `local function isGroundHit` was defined with). The lifter stores it in
        // `Function::name`; prefer it so a closure-valued local reads as its real
        // name instead of a generic `fn`. Falls back to `fn` when absent
        // (anonymous closures) or unusable.
        RValue::Closure(closure) => closure
            .function
            .lock()
            .name
            .as_deref()
            .and_then(sanitize)
            .or_else(|| Some("fn".to_string())),
        _ => None,
    }
}

fn string_literal(rvalue: &RValue) -> Option<&str> {
    if let RValue::Literal(Literal::String(bytes)) = rvalue {
        std::str::from_utf8(bytes).ok()
    } else {
        None
    }
}

fn index_key(index: &Index) -> Option<&str> {
    string_literal(&index.right)
}

fn global_name(rvalue: &RValue) -> Option<&str> {
    if let RValue::Global(global) = rvalue {
        std::str::from_utf8(&global.0).ok()
    } else {
        None
    }
}

fn class_name_hint(class_name: &str) -> Option<String> {
    match class_name {
        "BasePart" | "Part" | "MeshPart" | "UnionOperation" => Some("part".to_string()),
        "Script" | "LocalScript" => Some("script".to_string()),
        "GuiObject" => Some("guiObject".to_string()),
        "GuiButton" | "TextButton" | "ImageButton" => Some("button".to_string()),
        "TextLabel" => Some("label".to_string()),
        "ImageLabel" => Some("image".to_string()),
        "ParticleEmitter" => Some("emitter".to_string()),
        "PointLight" | "SpotLight" | "SurfaceLight" => Some("light".to_string()),
        "RemoteEvent" => Some("remoteEvent".to_string()),
        "RemoteFunction" => Some("remoteFunction".to_string()),
        other => sanitize(other),
    }
}

fn class_hint_family(class_name: &str) -> String {
    match class_name {
        "BasePart" | "Part" | "MeshPart" | "UnionOperation" => "part".to_string(),
        "Script" | "LocalScript" | "ModuleScript" | "BaseScript" => "script".to_string(),
        "ParticleEmitter" | "Beam" | "Trail" => "effect".to_string(),
        "GuiObject" | "GuiButton" | "TextButton" | "ImageButton" | "Frame" | "ScrollingFrame"
        | "TextLabel" | "ImageLabel" => "guiObject".to_string(),
        "PointLight" | "SpotLight" | "SurfaceLight" => "light".to_string(),
        other => other.to_string(),
    }
}

fn table_value_name(value: &RValue) -> Option<&str> {
    match value {
        RValue::Index(index) => index_key(index),
        RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call))
            if method_call.method == "GetService"
                || method_call.method.starts_with("FindFirst")
                || method_call.method.starts_with("WaitFor") =>
        {
            method_call.arguments.first().and_then(string_literal)
        }
        _ => None,
    }
}

fn table_collection_hint(table: &Table) -> Option<String> {
    let field_names = table
        .0
        .iter()
        .filter_map(|(key, value)| {
            if key.is_some() {
                return None;
            }
            table_value_name(value)
        })
        .collect::<Vec<_>>();

    if field_names.len() < 2 {
        return None;
    }

    let known_target_folder_count = field_names
        .iter()
        .filter(|name| {
            matches!(
                name.to_ascii_lowercase().as_str(),
                "npcs"
                    | "debris"
                    | "animals"
                    | "characters"
                    | "farm"
                    | "farms"
                    | "plots"
                    | "plants"
                    | "clouds"
                    | "folders"
            )
        })
        .count();

    if known_target_folder_count >= 2 {
        return Some("TargetFolders".to_string());
    }

    if field_names
        .iter()
        .any(|name| name.to_ascii_lowercase().contains("folder"))
    {
        return Some("Folders".to_string());
    }

    None
}

fn strip_script_suffixes(mut name: &str) -> &str {
    loop {
        let Some((stem, suffix)) = name.rsplit_once('.') else {
            return name;
        };
        match suffix.to_ascii_lowercase().as_str() {
            "lua" | "luau" | "client" | "server" | "module" => name = stem,
            _ => return name,
        }
    }
}

fn script_module_hint(script_name: &str) -> Option<String> {
    let trimmed = script_name.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts = trimmed
        .split(['\\', '/'])
        .filter(|part| !part.trim().is_empty())
        .flat_map(|part| {
            let stripped = strip_script_suffixes(part.trim()).trim();
            stripped
                .split('.')
                .filter(|part| !part.trim().is_empty())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut stem = parts.pop().unwrap_or(trimmed).trim();

    if stem.eq_ignore_ascii_case("init") {
        stem = parts
            .pop()
            .map(|part| strip_script_suffixes(part.trim()).trim())
            .unwrap_or("");
    }

    if stem.is_empty()
        || matches!(
            stem.to_ascii_lowercase().as_str(),
            "init" | "client" | "server" | "script" | "localscript" | "modulescript"
        )
    {
        return None;
    }

    sanitize_pascal(stem)
}

fn state_name_from_setter(setter: &str) -> Option<String> {
    let rest = setter.strip_prefix("set")?;
    if rest.is_empty() || !rest.as_bytes()[0].is_ascii_uppercase() {
        return None;
    }
    sanitize(rest)
}

fn setter_name_for_state(state: &str) -> Option<String> {
    let mut chars = state.chars();
    let first = chars.next()?;
    let mut setter = String::from("set");
    setter.push(first.to_ascii_uppercase());
    setter.extend(chars);
    sanitize_preserve(&setter)
}

fn callable_static_name(rvalue: &RValue) -> Option<&str> {
    match rvalue {
        RValue::Global(global) => std::str::from_utf8(&global.0).ok(),
        RValue::Index(index) => index_key(index),
        _ => None,
    }
}

fn protected_call_result_hint(rvalue: &RValue) -> Option<String> {
    let RValue::Index(index) = rvalue else {
        return None;
    };
    let key = index_key(index)?;
    let getter_name = key
        .strip_prefix("Get")
        .or_else(|| key.strip_prefix("Find"))
        .filter(|rest| !rest.is_empty())?;
    sanitize(getter_name)
}

/// Names for the variables of a generic `for`, inferred from the iterator.
fn iterator_names(right: &[RValue]) -> Option<Vec<&'static str>> {
    fn generator_global(rvalue: &RValue) -> Option<&[u8]> {
        match rvalue {
            RValue::Global(global) => Some(global.0.as_slice()),
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                generator_global(&call.value)
            }
            _ => None,
        }
    }
    fn generator_method(rvalue: &RValue) -> Option<&str> {
        match rvalue {
            RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
                Some(method_call.method.as_str())
            }
            RValue::Call(call) | RValue::Select(Select::Call(call))
                if generator_global(&call.value)
                    .is_some_and(|name| name == b"ipairs" || name == b"pairs") =>
            {
                call.arguments.first().and_then(generator_method)
            }
            RValue::Call(call) | RValue::Select(Select::Call(call)) => {
                generator_method(&call.value)
            }
            _ => None,
        }
    }
    for rvalue in right {
        if let Some(method) = generator_method(rvalue) {
            match method {
                "GetChildren" => return Some(vec!["i", "child"]),
                "GetDescendants" => return Some(vec!["i", "descendant"]),
                _ => {}
            }
        }
        if let Some(name) = generator_global(rvalue) {
            if name == b"ipairs" {
                return Some(vec!["i", "v"]);
            }
            if name == b"pairs" || name == b"next" {
                return Some(vec!["k", "v"]);
            }
        }
    }
    None
}

/// Singular form of a (presumed) plural collection identifier, used to name a
/// generic-for element variable after the collection it iterates
/// (`crops` -> `crop`, `MAIL_BODY_KEYS`/`keys` -> `key`). Returns `None` when the
/// word is not a clear plural so the caller falls back to the default iterator
/// name rather than inventing a non-word (`status` -> never `statu`).
fn singularize(name: &str) -> Option<String> {
    // Non-ASCII identifiers can't be cleanly singularized, and the byte slicing
    // below assumes 1 byte == 1 char — guarding here keeps it panic-safe.
    if !name.is_ascii() {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    // Words that LOOK plural (end in `s`) but aren't, or whose singular our rules
    // would mangle into a non-word (Latin irregulars, `consonant+ie+s`). Refusing
    // them falls back to the default iterator name rather than inventing junk.
    const NON_PLURAL: &[&str] = &[
        // singular nouns ending in s/is/us/ss
        "status", "data", "address", "class", "process", "bonus", "physics", "analysis",
        "axis", "props", "series", "species", "news", "progress", "pass", "mass", "boss",
        "loss", "glass", "lens", "gas", "basis", "access", "success", "focus", "bias",
        "canvas", "radius", "virus", "index", "this", "kudos",
        // Latin/irregular plurals our drop-`s`/`-es` rules would mangle
        "indices", "vertices", "matrices", "analyses", "axes", "crises", "bases",
        "theses", "diagnoses", "hypotheses", "parentheses",
        // singular ends in `ie`, so `ies`->`y` is wrong (movie -> "movy")
        "movies", "cookies", "zombies", "rookies", "newbies", "selfies", "calories",
        "brownies",
    ];
    if NON_PLURAL.contains(&lower.as_str()) {
        return None;
    }

    let n = name.len();
    let bytes = name.as_bytes();
    let singular = if let Some(stem) = name.strip_suffix("ies") {
        // entries -> entry, but only `consonant + ies` and a stem >= 3 chars
        // (rejects ties/dies/lies/pies and the like).
        let prev = stem.chars().last();
        if stem.len() < 3 || prev.is_some_and(|c| "aeiou".contains(c.to_ascii_lowercase())) {
            return None;
        }
        format!("{}y", stem)
    } else if lower.ends_with("ses")
        || lower.ends_with("xes")
        || lower.ends_with("zes")
        || lower.ends_with("ches")
        || lower.ends_with("shes")
    {
        // boxes -> box, matches -> match.
        name[..n - 2].to_string()
    } else if lower.ends_with("oes") {
        // heroes->hero needs drop-`es` but shoes->shoe needs drop-`s`; ambiguous,
        // so refuse rather than risk a non-word.
        return None;
    } else if name.ends_with('s') {
        // crops -> crop, lines -> line, markers -> marker. Reject `ss`/`is`/`us`,
        // which are almost never plural markers (address, axis, bonus, ...).
        if n < 2 {
            return None;
        }
        let prev = bytes[n - 2].to_ascii_lowercase();
        if prev == b's' || prev == b'i' || prev == b'u' {
            return None;
        }
        name[..n - 1].to_string()
    } else {
        return None;
    };

    if singular.eq_ignore_ascii_case(name) || singular.len() < 2 {
        return None;
    }
    sanitize(&singular)
}

/// `pairs(x)`/`ipairs(x)`/`next(x)` -> `x`; anything else is returned unchanged.
/// Used to look through a generic-for iterator wrapper at the real collection.
fn unwrap_iter_arg(rvalue: &RValue) -> &RValue {
    if let RValue::Call(call) | RValue::Select(Select::Call(call)) = rvalue
        && let RValue::Global(global) = &*call.value
        && matches!(global.0.as_slice(), b"pairs" | b"ipairs" | b"next")
        && let Some(arg) = call.arguments.first()
    {
        return arg;
    }
    rvalue
}

/// `react.createElement(...)` / `React.createElement(...)` / `e(...)` where `e`
/// is a local aliased to `*.createElement`. The React element constructor is the
/// signal that a filled table is a `children` map and a function is a component.
fn is_create_element_call(rvalue: &RValue, aliases: &FxHashSet<usize>) -> bool {
    let call = match rvalue {
        RValue::Call(call) | RValue::Select(Select::Call(call)) => call,
        _ => return false,
    };
    match &*call.value {
        RValue::Index(index) => index_key(index) == Some("createElement"),
        RValue::Global(global) => global.0.as_slice() == b"createElement",
        RValue::Local(local) => aliases.contains(&local_ptr(local)),
        _ => false,
    }
}

/// `react.useRef(...)` / `React.useRef(...)` / `Roact.createRef()`.
fn is_use_ref_call(rvalue: &RValue) -> bool {
    let call = match rvalue {
        RValue::Call(call) | RValue::Select(Select::Call(call)) => call,
        _ => return false,
    };
    match &*call.value {
        RValue::Index(index) => matches!(index_key(index), Some("useRef") | Some("createRef")),
        RValue::Global(global) => matches!(global.0.as_slice(), b"useRef" | b"createRef"),
        _ => false,
    }
}

/// `onClose`, `onActivated`, `setVisible`, ... — a React callback/handler prop
/// key. The name itself is the source string, so a local stored under such a key
/// can safely take that name.
fn is_callback_key(key: &str) -> bool {
    ["on", "set"].iter().any(|prefix| {
        key.strip_prefix(prefix)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|c| c.is_ascii_uppercase())
    })
}

/// Does an rvalue (not descending into closures) contain a `createElement` call?
fn rvalue_contains_create_element(rvalue: &RValue, aliases: &FxHashSet<usize>) -> bool {
    is_create_element_call(rvalue, aliases)
        || rvalue
            .rvalues()
            .iter()
            .any(|child| rvalue_contains_create_element(child, aliases))
}

/// Does a function body render a React element (call `createElement` in its own
/// body, excluding nested closures)? Marks the enclosing function as a component,
/// which gates the `props` parameter heuristic.
fn uses_create_element(block: &Block, aliases: &FxHashSet<usize>) -> bool {
    block.0.iter().any(|statement| {
        statement
            .rvalues()
            .iter()
            .any(|rvalue| rvalue_contains_create_element(rvalue, aliases))
            || match statement {
                Statement::If(r#if) => {
                    uses_create_element(&r#if.then_block.lock(), aliases)
                        || uses_create_element(&r#if.else_block.lock(), aliases)
                }
                Statement::While(r#while) => uses_create_element(&r#while.block.lock(), aliases),
                Statement::Repeat(repeat) => uses_create_element(&repeat.block.lock(), aliases),
                Statement::NumericFor(numeric_for) => {
                    uses_create_element(&numeric_for.block.lock(), aliases)
                }
                Statement::GenericFor(generic_for) => {
                    uses_create_element(&generic_for.block.lock(), aliases)
                }
                _ => false,
            }
    })
}

/// Locals declared as `local x = <something>.createElement`, so a later `x(...)`
/// can be recognised as a React element constructor.
fn collect_create_element_aliases(block: &mut Block, aliases: &mut FxHashSet<usize>) {
    for statement in &mut block.0 {
        if let Statement::Assign(assign) = &*statement
            && assign.prefix
        {
            for (lvalue, rvalue) in assign.left.iter().zip(assign.right.iter()) {
                if let Some(local) = lvalue.as_local()
                    && let RValue::Index(index) = rvalue
                    && index_key(index) == Some("createElement")
                {
                    aliases.insert(local_ptr(local));
                }
            }
        }

        let mut functions = Vec::new();
        statement.post_traverse_values(&mut |value| -> Option<()> {
            if let Either::Right(RValue::Closure(closure)) = value {
                functions.push(closure.function.clone());
            }
            None
        });
        for function in functions {
            collect_create_element_aliases(&mut function.lock().body, aliases);
        }
        match &*statement {
            Statement::If(r#if) => {
                collect_create_element_aliases(&mut r#if.then_block.lock(), aliases);
                collect_create_element_aliases(&mut r#if.else_block.lock(), aliases);
            }
            Statement::While(r#while) => {
                collect_create_element_aliases(&mut r#while.block.lock(), aliases)
            }
            Statement::Repeat(repeat) => {
                collect_create_element_aliases(&mut repeat.block.lock(), aliases)
            }
            Statement::NumericFor(numeric_for) => {
                collect_create_element_aliases(&mut numeric_for.block.lock(), aliases)
            }
            Statement::GenericFor(generic_for) => {
                collect_create_element_aliases(&mut generic_for.block.lock(), aliases)
            }
            _ => {}
        }
    }
}

/// Per-local usage facts gathered in one whole-tree pass before naming, so the
/// scoring heuristics (props/children/result/ref/callback/iterator) can consult
/// complete information regardless of statement order.
#[derive(Default, Clone)]
struct LocalUsage {
    /// Distinct string field keys read as `local.Field` / `local["Field"]`.
    string_fields_read: FxHashSet<String>,
    /// Invoked directly: `local(...)`.
    used_as_callee: bool,
    /// Indexed by a non-string-literal key (`local[i]`), i.e. array/map access.
    dynamic_indexed: bool,
    /// Iterated over by a generic-for.
    iterated: bool,
    /// Some `local.Field = ...` write (a mutated record is `state`, not `props`).
    field_written: bool,
    /// `local[k] = ...` keyed write occurred inside a loop (an accumulator fill).
    keyed_assign_in_loop: bool,
    /// How many fills store a `createElement(...)` value (`local[k] = e(...)` or
    /// `table.insert(local, e(...))`) — a React children map needs >=2, or >=1 in
    /// a loop. Counting elements (not total assigns) avoids mislabeling a config
    /// table that merely holds one nested element as `children`.
    create_element_fill_count: u32,
    create_element_fill_in_loop: bool,
    /// `table.insert(local, ...)` inside a loop.
    table_insert_in_loop: bool,
    /// Returned from its enclosing block/function.
    returned: bool,
    /// Stored as the value of a callback-shaped table field (`onClose = local`).
    callback_field_name: Option<String>,
    /// Implied type from a `typeof(x)`/`type(x)` guard, collapsed to a small
    /// stable tag (see [`type_tag`]). `None` until a guard is seen.
    typeof_type: Option<&'static str>,
    /// Two *different* type guards were seen on this local — it is genuinely
    /// polymorphic, so the namer refuses to name it from a type.
    typeof_conflict: bool,
    /// Used as the receiver of an instance-shaped method call (see
    /// [`INSTANCE_METHODS`]).
    instance_method_seen: bool,
}

fn note_call_usage(
    call: &Call,
    in_loop: bool,
    aliases: &FxHashSet<usize>,
    usage: &mut FxHashMap<usize, LocalUsage>,
) {
    if let RValue::Local(local) = &*call.value {
        usage.entry(local_ptr(local)).or_default().used_as_callee = true;
    }
    if in_loop
        && let RValue::Index(index) = &*call.value
        && index_key(index) == Some("insert")
        && let RValue::Global(global) = &*index.left
        && global.0.as_slice() == b"table"
        && let Some(RValue::Local(local)) = call.arguments.first()
    {
        let pushes_element = call
            .arguments
            .get(1)
            .is_some_and(|value| is_create_element_call(value, aliases));
        let entry = usage.entry(local_ptr(local)).or_default();
        entry.table_insert_in_loop = true;
        // `table.insert(children, e(...))` in a loop is an array-style children map.
        if pushes_element {
            entry.create_element_fill_count += 1;
            entry.create_element_fill_in_loop = true;
        }
    }
}

/// Roblox/Lua `typeof`/`type` strings collapsed to a small stable tag. Only
/// `string`, `number`, `Instance` and `function` are nameable; every other
/// recognised type (and anything unrecognised) collapses to `"other"`, so two
/// *different* type guards on the same local register as a conflict without us
/// having to store arbitrary strings.
fn type_tag(type_name: &str) -> &'static str {
    match type_name {
        "string" => "string",
        "number" => "number",
        "Instance" => "Instance",
        "function" => "function",
        _ => "other",
    }
}

/// The `RcLocal` of a bare `typeof(x)` / `type(x)` call, or `None`. Requires
/// exactly one argument that is a plain local, so `typeof(x.Field)`,
/// `typeof(f())` and `typeof(a) == typeof(b)` are all rejected.
fn typeof_call_local(rvalue: &RValue) -> Option<&RcLocal> {
    let call = match rvalue {
        RValue::Call(call) | RValue::Select(Select::Call(call)) => call,
        _ => return None,
    };
    let RValue::Global(global) = &*call.value else {
        return None;
    };
    if global.0.as_slice() != b"typeof" && global.0.as_slice() != b"type" {
        return None;
    }
    if call.arguments.len() != 1 {
        return None;
    }
    match &call.arguments[0] {
        RValue::Local(local) => Some(local),
        _ => None,
    }
}

/// `typeof(x) == "T"` / `typeof(x) ~= "T"` (either operand order) -> `(x, "T")`.
/// Both `==` and `~=` are read as "x is intended to be of type T": a `~=` guard
/// is the idiomatic early-return form (`if typeof(x) ~= "string" then return end`)
/// and still tells us the param's type.
fn type_guard_parts(binary: &Binary) -> Option<(&RcLocal, &str)> {
    if let Some(local) = typeof_call_local(&binary.left)
        && let Some(type_name) = string_literal(&binary.right)
    {
        return Some((local, type_name));
    }
    if let Some(local) = typeof_call_local(&binary.right)
        && let Some(type_name) = string_literal(&binary.left)
    {
        return Some((local, type_name));
    }
    None
}

/// Instance-shaped methods: a local used as the receiver of one of these is very
/// likely a Roblox `Instance`. `IsA` is deliberately excluded — it routes through
/// `set_isa_hint`, which yields the more specific class word at a higher score.
const INSTANCE_METHODS: &[&str] = &[
    "FindFirstChild",
    "FindFirstChildOfClass",
    "FindFirstChildWhichIsA",
    "FindFirstAncestor",
    "WaitForChild",
    "GetChildren",
    "GetDescendants",
    "GetPivot",
    "PivotTo",
    "Clone",
    "Destroy",
    "GetAttribute",
    "SetAttribute",
    "GetFullName",
    "IsDescendantOf",
    "ScaleTo",
    "GetBoundingBox",
];

/// Ordered parameter names for a Roblox event's `:Connect` callback. A `None`
/// slot keeps the param's default name. Only conservative, well-known signatures
/// are listed; overloaded/arbitrary ones (`Changed`, `OnClientEvent`, ...) are
/// intentionally absent so we never invent a misleading name.
fn event_signature(event: &str) -> Option<&'static [Option<&'static str>]> {
    Some(match event {
        "Heartbeat" | "RenderStepped" | "PreSimulation" | "PostSimulation" | "PreRender"
        | "PreAnimation" => &[Some("dt")],
        "Stepped" => &[Some("time"), Some("dt")],
        "InputBegan" | "InputEnded" | "InputChanged" => &[Some("input"), Some("gameProcessed")],
        "ChildAdded" | "ChildRemoved" => &[Some("child")],
        "DescendantAdded" | "DescendantRemoving" => &[Some("descendant")],
        "AncestryChanged" => &[None, Some("parent")],
        "CharacterAdded" | "CharacterRemoving" | "CharacterAppearanceLoaded" => &[Some("character")],
        "PlayerAdded" | "PlayerRemoving" => &[Some("player")],
        "Triggered" | "PromptTriggered" => &[Some("player")],
        "Touched" | "TouchEnded" => &[Some("otherPart")],
        _ => return None,
    })
}

/// Record that `local` is the receiver of an instance-shaped method call.
fn note_method_usage(method_call: &MethodCall, usage: &mut FxHashMap<usize, LocalUsage>) {
    if let RValue::Local(local) = &*method_call.value
        && INSTANCE_METHODS.contains(&method_call.method.as_str())
    {
        usage
            .entry(local_ptr(local))
            .or_default()
            .instance_method_seen = true;
    }
}

/// Record a `typeof(x)`/`type(x)` guard's implied type for `x`, flagging a
/// conflict if a different type was already seen (so the namer refuses rather
/// than guess).
fn note_type_guard(binary: &Binary, usage: &mut FxHashMap<usize, LocalUsage>) {
    if !matches!(
        binary.operation,
        BinaryOperation::Equal | BinaryOperation::NotEqual
    ) {
        return;
    }
    let Some((local, type_name)) = type_guard_parts(binary) else {
        return;
    };
    let tag = type_tag(type_name);
    let entry = usage.entry(local_ptr(local)).or_default();
    match entry.typeof_type {
        None => entry.typeof_type = Some(tag),
        Some(existing) if existing != tag => entry.typeof_conflict = true,
        _ => {}
    }
}

fn gather_usage(
    block: &mut Block,
    in_loop: bool,
    aliases: &FxHashSet<usize>,
    usage: &mut FxHashMap<usize, LocalUsage>,
) {
    for statement in &mut block.0 {
        // Expression-local facts: field reads/writes, callees, callback table fields.
        statement.post_traverse_values(&mut |value| -> Option<()> {
            match value {
                Either::Right(RValue::Index(index)) => {
                    if let RValue::Local(local) = &*index.left {
                        let entry = usage.entry(local_ptr(local)).or_default();
                        match string_literal(&index.right) {
                            Some(key) => {
                                entry.string_fields_read.insert(key.to_string());
                            }
                            None => entry.dynamic_indexed = true,
                        }
                    }
                }
                Either::Left(crate::LValue::Index(index)) => {
                    if let RValue::Local(local) = &*index.left {
                        let entry = usage.entry(local_ptr(local)).or_default();
                        match string_literal(&index.right) {
                            Some(_) => entry.field_written = true,
                            None => entry.dynamic_indexed = true,
                        }
                    }
                }
                Either::Right(RValue::Call(call)) => {
                    note_call_usage(call, in_loop, aliases, usage)
                }
                Either::Right(RValue::Table(table)) => {
                    for (key, val) in &table.0 {
                        if let (Some(key), RValue::Local(local)) = (key.as_ref(), val)
                            && let Some(key) = string_literal(key)
                            && is_callback_key(key)
                        {
                            usage
                                .entry(local_ptr(local))
                                .or_default()
                                .callback_field_name = Some(key.to_string());
                        }
                    }
                }
                Either::Right(RValue::MethodCall(method_call))
                | Either::Right(RValue::Select(Select::MethodCall(method_call))) => {
                    note_method_usage(method_call, usage);
                }
                Either::Right(RValue::Binary(binary)) => {
                    note_type_guard(binary, usage);
                }
                _ => {}
            }
            None
        });

        match &*statement {
            Statement::Call(call) => note_call_usage(call, in_loop, aliases, usage),
            Statement::MethodCall(method_call) => note_method_usage(method_call, usage),
            Statement::Assign(assign) => {
                for (lvalue, rvalue) in assign.left.iter().zip(assign.right.iter()) {
                    if let crate::LValue::Index(index) = lvalue
                        && let RValue::Local(local) = &*index.left
                    {
                        let create_element = is_create_element_call(rvalue, aliases);
                        let entry = usage.entry(local_ptr(local)).or_default();
                        if in_loop {
                            entry.keyed_assign_in_loop = true;
                        }
                        if create_element {
                            entry.create_element_fill_count += 1;
                            if in_loop {
                                entry.create_element_fill_in_loop = true;
                            }
                        }
                    }
                }
            }
            Statement::Return(ret) => {
                for value in &ret.values {
                    if let RValue::Local(local) = value {
                        usage.entry(local_ptr(local)).or_default().returned = true;
                    }
                }
            }
            Statement::GenericFor(generic_for) => {
                for rvalue in &generic_for.right {
                    if let RValue::Local(local) = unwrap_iter_arg(rvalue) {
                        usage.entry(local_ptr(local)).or_default().iterated = true;
                    }
                }
            }
            _ => {}
        }

        // Recurse: closures reset the loop context; loops set it.
        let mut functions = Vec::new();
        statement.post_traverse_values(&mut |value| -> Option<()> {
            if let Either::Right(RValue::Closure(closure)) = value {
                functions.push(closure.function.clone());
            }
            None
        });
        for function in functions {
            gather_usage(&mut function.lock().body, false, aliases, usage);
        }
        match &*statement {
            Statement::If(r#if) => {
                gather_usage(&mut r#if.then_block.lock(), in_loop, aliases, usage);
                gather_usage(&mut r#if.else_block.lock(), in_loop, aliases, usage);
            }
            Statement::While(r#while) => gather_usage(&mut r#while.block.lock(), true, aliases, usage),
            Statement::Repeat(repeat) => gather_usage(&mut repeat.block.lock(), true, aliases, usage),
            Statement::NumericFor(numeric_for) => {
                gather_usage(&mut numeric_for.block.lock(), true, aliases, usage)
            }
            Statement::GenericFor(generic_for) => {
                gather_usage(&mut generic_for.block.lock(), true, aliases, usage)
            }
            _ => {}
        }
    }
}

/// The local of a `local v` / `local v = nil` empty declaration (the shape a
/// `conditional_expressions` diamond temp is declared with). Mirrors that pass's
/// `candidate_decl` (conditional_expressions.rs:141).
fn empty_decl_local(statement: &Statement) -> Option<RcLocal> {
    let Statement::Assign(assign) = statement else {
        return None;
    };
    if !assign.prefix || assign.parallel || assign.left.len() != 1 {
        return None;
    }
    if !(assign.right.is_empty()
        || matches!(assign.right.as_slice(), [RValue::Literal(Literal::Nil)]))
    {
        return None;
    }
    assign.left[0].as_local().cloned()
}

/// Whether `block` is exactly `[local? assigned-to `local`]` — one non-prefix
/// single assignment to `local` (an `if`/`else` arm of a diamond). Mirrors
/// `single_local_assignment_value` (conditional_expressions.rs:165).
fn arm_assigns_only(block: &Block, local: &RcLocal) -> bool {
    let [Statement::Assign(assign)] = block.0.as_slice() else {
        return false;
    };
    !assign.prefix
        && !assign.parallel
        && assign.left.len() == 1
        && assign.right.len() == 1
        && assign.left[0].as_local() == Some(local)
}

/// Collect the locals that look like `conditional_expressions` ternary-collapse
/// candidates: a `local v` empty decl, an `if` immediately after whose then/else
/// arms each *solely* assign `v`, and a use of `v` in the *immediately* following
/// statement. Naming such a temp from an arm RHS would make `is_generated_temp(v)`
/// false and suppress the collapse (+lines), so it must keep its generated name.
/// The strict adjacency is what keeps a 3-write/1-read temp whose use is NOT
/// adjacent (so it never collapses) nameable — e.g.
/// `local cFrame; if .. end; local a; local b; use(cFrame*a*b)`.
///
/// This is a deliberately CONSERVATIVE SUPERSET of what the pass actually
/// collapses: it does not mirror the pass's `replaceable_direct_rvalue_read_count
/// == 1` / `classify_replaceable_use` / `contains_unsupported_value` /
/// `complexity_allowed` gates (conditional_expressions.rs:110-124). So a few
/// adjacent-but-non-collapsible shapes (an `if v then`/`v.f = x` use, a
/// Closure/VarArg or over-complex arm) are over-matched: naming is suppressed and
/// the local keeps its `vN` name even though it survives. That is a pure
/// readability trade (never a semantic change, never +lines) on the safe side —
/// suppressing a name can only ever ENABLE a collapse, never break one.
fn collect_collapse_candidates(block: &mut Block, out: &mut FxHashSet<usize>) {
    let len = block.0.len();
    for i in 0..len {
        if i + 2 < len
            && let Some(local) = empty_decl_local(&block.0[i])
            && let Statement::If(r#if) = &block.0[i + 1]
            && arm_assigns_only(&r#if.then_block.lock(), &local)
            && arm_assigns_only(&r#if.else_block.lock(), &local)
            && block.0[i + 2].values_read().iter().any(|r| **r == local)
        {
            out.insert(local_ptr(&local));
        }
    }
    // Recurse into every nested block (the diamond may sit inside a branch,
    // loop, or closure body).
    for statement in &mut block.0 {
        let mut functions = Vec::new();
        statement.post_traverse_values(&mut |value| -> Option<()> {
            if let Either::Right(RValue::Closure(closure)) = value {
                functions.push(closure.function.clone());
            }
            None
        });
        for function in functions {
            collect_collapse_candidates(&mut function.lock().body, out);
        }
        match statement {
            Statement::If(r#if) => {
                collect_collapse_candidates(&mut r#if.then_block.lock(), out);
                collect_collapse_candidates(&mut r#if.else_block.lock(), out);
            }
            Statement::While(r#while) => {
                collect_collapse_candidates(&mut r#while.block.lock(), out)
            }
            Statement::Repeat(repeat) => {
                collect_collapse_candidates(&mut repeat.block.lock(), out)
            }
            Statement::NumericFor(nf) => collect_collapse_candidates(&mut nf.block.lock(), out),
            Statement::GenericFor(gf) => collect_collapse_candidates(&mut gf.block.lock(), out),
            _ => {}
        }
    }
}

struct Namer {
    rename: bool,
    module_hint: Option<String>,
    /// Names that may not currently be assigned to a local: every global
    /// referenced in the program (added once in `collect`, kept reserved
    /// program-wide so a referenced global is never shadowed) plus the names of
    /// every local that is CURRENTLY in scope. Lexically scoped names are added
    /// when a scope is entered and released (via `release`) when it ends, so two
    /// locals in disjoint/sibling scopes may reuse the same base name while two
    /// simultaneously-visible locals never collide.
    reserved: FxHashSet<String>,
    /// Preferred base name for a local, keyed by `local_ptr`.
    hints: FxHashMap<usize, Hint>,
    /// Broader context names that are safer than a narrow type name when usage
    /// proves the same local may hold several unrelated Instance classes.
    context_hints: FxHashMap<usize, String>,
    /// Class families observed through `:IsA(...)`, used to avoid naming a
    /// mixed-class local after only the first branch that inspected it.
    isa_families: FxHashMap<usize, String>,
    /// Root locals declared from a table literal. A root-level final return of
    /// one of these locals can safely use the script/module name.
    module_table_locals: FxHashSet<usize>,
    /// Locals that have already been named.
    named: FxHashSet<usize>,
    /// Locals bound to a closure. Such a local keeps its (function-derived) name
    /// even when unused, so a recovered local function whose calls were inlined
    /// away by the Luau -O2 compiler reads as itself rather than `_`.
    closure_locals: FxHashSet<usize>,
    /// Per-local usage facts gathered before naming (see `LocalUsage`).
    usage: FxHashMap<usize, LocalUsage>,
    /// Locals aliased to `*.createElement` (see `collect_create_element_aliases`).
    create_element_aliases: FxHashSet<usize>,
    /// Read/write/capture counts, computed with the EXACT same routine
    /// (`inline_temps::collect_usage`) that `inline_temps` and
    /// `conditional_expressions` consume, so `is_collapse_candidate` agrees
    /// bit-for-bit with the gate those passes apply. Keyed by `local_ptr` (the
    /// Arc *address*), NOT `RcLocal` — holding an `RcLocal` here would keep a
    /// strong Arc clone alive for every local, inflating `Arc::count` and
    /// breaking `name_one`'s unused-local detection (`Arc::count == 1` -> `_`).
    /// See the note on `local_ptr`.
    counts: FxHashMap<usize, Usage>,
    /// Locals matching the EXACT structural shape `conditional_expressions`
    /// collapses (`local v; if c then v=A else v=B end; use(v)` — adjacent).
    /// See `collect_collapse_candidates`.
    collapse_candidates: FxHashSet<usize>,
}

impl Namer {
    /// Whether `local` is a `conditional_expressions` ternary-collapse candidate
    /// — the exact gate that pass applies (`reads == 1 && writes == 3 &&
    /// !captured`, conditional_expressions.rs:99). Such a temp (`local v; if c
    /// then v = A else v = B end; use(v)`) must keep its generated `vN` name so
    /// `is_generated_temp(v)` stays true and the collapse fires; naming it from
    /// an arm RHS would suppress the collapse and leave the expanded if-else
    /// (+lines). Counts are stable between here and that pass (only recover_methods
    /// and a movable-temp inline run in between, neither of which alters a
    /// 3-write temp's read/write counts).
    fn is_collapse_candidate(&self, local: &RcLocal) -> bool {
        // Both the count gate (what the pass tests) AND the exact adjacency
        // structure must hold. The structural set keeps a 3-write/1-read temp
        // whose use is NOT adjacent (so it never actually collapses) nameable.
        self.collapse_candidates.contains(&local_ptr(local))
            && self
                .counts
                .get(&local_ptr(local))
                .is_some_and(|u| u.reads == 1 && u.writes == 3 && !u.captured)
    }

    fn set_hint(&mut self, local: &RcLocal, name: String, score: u8) {
        let ptr = local_ptr(local);
        let replace = match self.hints.get(&ptr) {
            Some(existing) => score > existing.score,
            None => true,
        };
        if replace {
            self.hints.insert(ptr, Hint { name, score });
        }
    }

    fn set_hint_str(&mut self, local: &RcLocal, name: &'static str, score: u8) {
        self.set_hint(local, name.to_string(), score);
    }

    fn set_context_hint_str(&mut self, local: &RcLocal, name: &'static str, score: u8) {
        self.context_hints
            .insert(local_ptr(local), name.to_string());
        self.set_hint_str(local, name, score);
    }

    fn set_isa_hint(&mut self, local: &RcLocal, class_name: &str) {
        let Some(hint) = class_name_hint(class_name) else {
            return;
        };

        let ptr = local_ptr(local);
        let family = class_hint_family(class_name);
        if let Some(existing_family) = self.isa_families.get(&ptr) {
            if existing_family != &family {
                if let Some(fallback) = self.context_hints.get(&ptr).cloned() {
                    self.set_hint(local, fallback, 56);
                }
                return;
            }
            if family == "script"
                && let Some(existing) = self.hints.get(&ptr)
                && existing.score == 55
                && existing.name != hint
            {
                self.set_hint_str(local, "script", 56);
                return;
            }
        } else {
            self.isa_families.insert(ptr, family);
        }

        self.set_hint(local, hint, 55);
    }

    fn hint_name(&self, local: &RcLocal) -> Option<&str> {
        self.hints
            .get(&local_ptr(local))
            .map(|hint| hint.name.as_str())
    }

    fn local_known_name(&self, local: &RcLocal) -> Option<String> {
        current_name(local).or_else(|| self.hint_name(local).map(str::to_string))
    }

    fn callable_name(&self, rvalue: &RValue) -> Option<String> {
        callable_static_name(rvalue)
            .map(str::to_string)
            .or_else(|| {
                if let RValue::Local(local) = rvalue {
                    self.local_known_name(local)
                } else {
                    None
                }
            })
    }

    fn is_use_state_call(&self, call: &Call) -> bool {
        self.callable_name(&call.value)
            .is_some_and(|name| name == "useState")
    }

    fn apply_pcall_tuple_hints(&mut self, assign: &crate::Assign, call: &Call, left_start: usize) {
        if self.callable_name(&call.value).as_deref() != Some("pcall")
            || assign.left.len() < left_start + 2
        {
            return;
        }

        if let Some(status) = assign.left[left_start].as_local() {
            self.set_hint_str(status, "success", 85);
        }

        let result_hint = call
            .arguments
            .first()
            .and_then(|callable| {
                if global_name(callable) == Some("require") {
                    call.arguments.get(1).and_then(base_name_of)
                } else {
                    protected_call_result_hint(callable)
                }
            })
            .unwrap_or_else(|| "result".to_string());

        if let Some(result) = assign.left[left_start + 1].as_local() {
            self.set_hint(result, result_hint, 80);
        }
    }

    fn apply_use_state_tuple_hints(
        &mut self,
        assign: &crate::Assign,
        call: &Call,
        left_start: usize,
    ) {
        if !self.is_use_state_call(call) || assign.left.len() < left_start + 2 {
            return;
        }

        let Some(state) = assign.left[left_start].as_local() else {
            return;
        };
        let Some(setter) = assign.left[left_start + 1].as_local() else {
            return;
        };

        if let Some(setter_name) = self.local_known_name(setter)
            && let Some(state_name) = state_name_from_setter(&setter_name)
        {
            self.set_hint(state, state_name, 95);
            self.set_hint(setter, setter_name, 95);
            return;
        }

        if let Some(state_name) = self.local_known_name(state)
            && let Some(setter_name) = setter_name_for_state(&state_name)
        {
            self.set_hint(state, state_name, 95);
            self.set_hint(setter, setter_name, 95);
            return;
        }

        self.set_hint_str(state, "state", 70);
        self.set_hint_str(setter, "setState", 70);
    }

    fn collect_method_usage(&mut self, method_call: &MethodCall) {
        let RValue::Local(local) = &*method_call.value else {
            return;
        };

        match method_call.method.as_str() {
            "GetDescendants" => self.set_hint_str(local, "folder", 55),
            "IsA" => {
                if let Some(class_name) = method_call.arguments.first().and_then(string_literal) {
                    self.set_isa_hint(local, class_name);
                }
            }
            _ => {}
        }
    }

    /// Parent-qualified name for a *generic* guarded lookup
    /// (`folder and folder:FindFirstChild("Client")`, where `folder` is already
    /// named `plantedSeeds`, yields `plantedSeedsClient`). Returns the qualified
    /// name and its score, or `None` to fall back to the bare child name. Mirrors
    /// how the original source disambiguates colliding generic children
    /// (ground truth: `plantClientFolder`/`potClientFolder`). Score 63 sits just
    /// above the bare lookup (60) and the callback hint (62) and below every
    /// string-anchored hint (70+).
    fn guarded_lookup_qualified_hint(&self, rvalue: &RValue) -> Option<(String, u8)> {
        let RValue::Binary(binary) = rvalue else {
            return None;
        };
        // Same lookup extraction as Layer 1, so an `... or default` tail is peeled
        // here too (`placedPots and placedPots:FindFirstChild("Server") or nil`
        // still qualifies to `placedPotsServer`).
        let method_call = binary_lookup_method_call(binary)?;
        let child = method_call_hint(method_call)?;
        if !is_generic_lookup_child(&child) {
            return None;
        }
        // The receiver must be a plain local with a real (non-default) name that
        // doesn't already carry the child word, else qualifying adds nothing.
        let RValue::Local(receiver) = &*method_call.value else {
            return None;
        };
        let parent = self.local_known_name(receiver)?;
        if is_default_name(&parent) || name_ends_with_word(&parent, &child) {
            return None;
        }
        let qualified = sanitize(&format!("{}{}", parent, capitalize_first(&child)))?;
        Some((qualified, 63))
    }

    /// A local bound to a boolean-predicate call (`local v = isGraphicsDisabled(x)`)
    /// reads as the predicate's subject (`graphicsDisabled`), matching how source
    /// names such results (§2.7 Layer A). Only a direct call (not a method call)
    /// whose callee resolves to an `is`/`has` predicate name qualifies; non-predicate
    /// calls and a multi-return call's extra slots fall through untouched. The callee
    /// is usually a recovered `local function isX` reference, so resolution needs
    /// `callable_name` (its name lives on the closure hint set earlier in this
    /// top-down collect); a callee whose name isn't yet known is a safe no-op.
    fn predicate_call_hint(&self, rvalue: &RValue) -> Option<String> {
        let (RValue::Call(call) | RValue::Select(Select::Call(call))) = rvalue else {
            return None;
        };
        let name = self.callable_name(&call.value)?;
        sanitize(strip_predicate_prefix(&name)?)
    }

    /// Singularize a generic-for element variable after the collection it
    /// iterates (`for index, crop in crops`). Higher priority (47) than the
    /// hardcoded `child`/`descendant` context names because it is derived from a
    /// real source identifier rather than guessed.
    fn collection_element_name(&self, right: &[RValue]) -> Option<String> {
        for rvalue in right {
            let name = match unwrap_iter_arg(rvalue) {
                RValue::Local(local) => self.local_known_name(local),
                RValue::Index(index) => index_key(index).map(str::to_string),
                _ => None,
            };
            if let Some(name) = name
                && let Some(singular) = singularize(&name)
            {
                return Some(singular);
            }
        }
        None
    }

    /// A local initialized to a table literal that is filled with React elements
    /// (`children`, score 48) or filled in a loop and returned (`result`, 35).
    fn children_or_result_hint(&mut self, local: &RcLocal) {
        let (children, result) = match self.usage.get(&local_ptr(local)) {
            Some(usage) => (
                usage.create_element_fill_count >= 2 || usage.create_element_fill_in_loop,
                (usage.keyed_assign_in_loop || usage.table_insert_in_loop) && usage.returned,
            ),
            None => (false, false),
        };
        if children {
            self.set_hint_str(local, "children", 48);
        } else if result {
            self.set_hint_str(local, "result", 35);
        }
    }

    /// A local assigned from `useRef`/`createRef` reads as `ref`. Source type
    /// annotations that would yield `frameRef`/`mountedRef` are gone in bytecode,
    /// so we emit the honest generic name rather than guess a stem.
    fn ref_hint(&mut self, local: &RcLocal, rvalue: &RValue) {
        if is_use_ref_call(rvalue) {
            self.set_hint_str(local, "ref", 50);
        }
    }

    /// A local stored under a callback-shaped table field (`onClose = local`)
    /// takes that key as its name (score 62). The key is a literal source string,
    /// so this is safe; it still yields to `useState` (95) and friends.
    fn callback_hint(&mut self, local: &RcLocal) {
        let key = self
            .usage
            .get(&local_ptr(local))
            .and_then(|usage| usage.callback_field_name.clone());
        if let Some(key) = key
            && let Some(name) = sanitize_preserve(&key)
        {
            self.set_hint(local, name, 62);
        }
    }

    /// A component parameter read as a record of named fields reads as `props`
    /// (score 50). Gated hard: the enclosing function must render an element, and
    /// the param must look like a read-only record (>=3 distinct string fields,
    /// never invoked, indexed, iterated, mutated, or `self`-like).
    fn props_param_hint(&mut self, param: &RcLocal, function_renders_element: bool) {
        if !function_renders_element {
            return;
        }
        let qualifies = match self.usage.get(&local_ptr(param)) {
            Some(usage) => {
                let distinct = usage.string_fields_read.len();
                let underscore = usage
                    .string_fields_read
                    .iter()
                    .filter(|field| field.starts_with('_'))
                    .count();
                distinct >= 3
                    && underscore * 2 <= distinct
                    && !usage.used_as_callee
                    && !usage.dynamic_indexed
                    && !usage.iterated
                    && !usage.field_written
            }
            None => false,
        };
        if qualifies {
            self.set_hint_str(param, "props", 50);
        }
    }

    /// Low-confidence presentational names inferred from how a *parameter* is used
    /// (§2.1). Every score sits below `props`/`callback`/`isa`, so these only fill
    /// a slot that would otherwise default to `p`, and a neutral hypernym is
    /// preferred over a guess — the goal is "better than `p`, never misleading".
    /// Signals are applied in strict precedence; the first match returns and the
    /// rest are skipped, so no two ever race on the same param.
    fn usage_param_hint(&mut self, param: &RcLocal) {
        let Some(usage) = self.usage.get(&local_ptr(param)) else {
            return;
        };
        // A `.UserId`/`.Character`/`.DisplayName` read is a strong Player tell.
        let is_player = ["UserId", "Character", "DisplayName"]
            .iter()
            .any(|field| usage.string_fields_read.contains(*field));
        let instance_shaped = usage.instance_method_seen;
        let typeof_conflict = usage.typeof_conflict;
        let typeof_type = usage.typeof_type;

        // Player is checked first, ahead of the conflict/contradiction guards
        // below: a `.UserId`/`.Character`/`.DisplayName` read is a near-certain
        // Player tell that outranks any noisy `typeof` evidence on the same param.
        if is_player {
            self.set_hint_str(param, "player", 44);
            return;
        }
        // Checked against multiple types -> genuinely polymorphic -> refuse.
        if typeof_conflict {
            return;
        }
        if instance_shaped {
            // typeof says scalar but it is used like an Instance -> contradiction.
            if matches!(typeof_type, Some("string") | Some("number")) {
                return;
            }
            self.set_hint_str(param, "instance", 42);
            return;
        }
        match typeof_type {
            Some("Instance") => self.set_hint_str(param, "instance", 41),
            Some("string") | Some("number") => self.set_hint_str(param, "value", 40),
            Some("function") => self.set_hint_str(param, "callback", 39),
            _ => {}
        }
    }

    /// Name an event callback's parameters from the event's known signature
    /// (`RunService.Heartbeat:Connect(function(dt) ... end)`). These are
    /// documented API conventions (near-deterministic), but still scored low so an
    /// existing stronger hint (e.g. an `:IsA` class word) wins.
    fn event_callback_hint(&mut self, method_call: &MethodCall) {
        if !matches!(
            method_call.method.as_str(),
            "Connect" | "Once" | "ConnectParallel"
        ) {
            return;
        }
        let RValue::Index(index) = &*method_call.value else {
            return;
        };
        let Some(event) = index_key(index) else {
            return;
        };
        let Some(signature) = event_signature(event) else {
            return;
        };
        let Some(RValue::Closure(closure)) = method_call.arguments.first() else {
            return;
        };
        let function = closure.function.lock();
        for (i, slot) in signature.iter().enumerate() {
            let Some(name) = *slot else {
                continue;
            };
            if let Some(param) = function.parameters.get(i) {
                // p0 of a known event is high-precision; later slots are weaker
                // synonyms (gameProcessed/parent/...), so they sit lower.
                self.set_hint_str(param, name, if i == 0 { 46 } else { 38 });
            }
        }
    }

    /// Name the two parameters of a `table.sort` comparator `a`/`b`, matching the
    /// near-universal source convention for sort predicates.
    fn comparator_hint(&mut self, call: &Call) {
        let RValue::Index(index) = &*call.value else {
            return;
        };
        if index_key(index) != Some("sort") {
            return;
        }
        let RValue::Global(global) = &*index.left else {
            return;
        };
        if global.0.as_slice() != b"table" {
            return;
        }
        let Some(RValue::Closure(closure)) = call.arguments.get(1) else {
            return;
        };
        let function = closure.function.lock();
        for (i, name) in ["a", "b"].into_iter().enumerate() {
            if let Some(param) = function.parameters.get(i) {
                self.set_hint_str(param, name, 45);
            }
        }
    }

    /// Reserve `base` if free, otherwise `base2`, `base3`, ... Returns the chosen
    /// name and records it in `scope` so it can be released when that scope ends.
    fn unique(&mut self, base: &str, scope: &mut Vec<String>) -> String {
        let name = if !self.reserved.contains(base) {
            base.to_string()
        } else {
            let mut counter = 2;
            loop {
                let candidate = format!("{}{}", base, counter);
                if !self.reserved.contains(&candidate) {
                    break candidate;
                }
                counter += 1;
            }
        };
        self.reserved.insert(name.clone());
        scope.push(name.clone());
        name
    }

    /// Release the names a scope reserved, freeing them for reuse by sibling
    /// scopes. Globals are never passed here, so they stay reserved program-wide.
    fn release(&mut self, scope: Vec<String>) {
        for name in scope {
            self.reserved.remove(&name);
        }
    }

    fn name_one(&mut self, local: &RcLocal, default_prefix: &str, scope: &mut Vec<String>) {
        let ptr = local_ptr(local);
        let mut lock = local.0 .0.lock();
        if !self.named.insert(ptr) {
            if let Some(name) = &lock.0
                && name != "_"
                && self.reserved.insert(name.clone())
            {
                scope.push(name.clone());
            }
            return;
        }
        if !(self.rename || lock.0.is_none()) {
            return;
        }
        // An unused local (its only reference is the declaration itself) is named
        // `_`, which is idiomatic and needs no uniqueness handling — UNLESS it is
        // a recovered local function (closure-bound) whose calls were inlined away
        // by the Luau -O2 compiler, which we keep named so it reads as itself.
        if Arc::count(&local.0 .0) == 1 {
            if self.closure_locals.contains(&ptr)
                && let Some(hint) = self.hints.get(&ptr).map(|hint| hint.name.clone())
            {
                lock.0 = Some(self.unique(&hint, scope));
            } else {
                lock.0 = Some("_".to_string());
            }
            return;
        }
        let base = self
            .hints
            .get(&ptr)
            .map(|hint| hint.name.clone())
            .unwrap_or_else(|| default_prefix.to_string());
        lock.0 = Some(self.unique(&base, scope));
    }

    /// First pass: gather reserved globals and per-local naming hints.
    fn collect(&mut self, block: &mut Block, is_root: bool) {
        for statement in &mut block.0 {
            let mut globals: Vec<String> = Vec::new();
            let mut functions = Vec::new();
            statement.post_traverse_values(&mut |value| -> Option<()> {
                match value {
                    Either::Right(RValue::Global(global)) => {
                        if let Ok(name) = std::str::from_utf8(&global.0) {
                            globals.push(name.to_string());
                        }
                    }
                    Either::Left(crate::LValue::Global(global)) => {
                        if let Ok(name) = std::str::from_utf8(&global.0) {
                            globals.push(name.to_string());
                        }
                    }
                    Either::Right(RValue::Closure(closure)) => {
                        functions.push(closure.function.clone());
                    }
                    Either::Right(RValue::MethodCall(method_call))
                    | Either::Right(RValue::Select(Select::MethodCall(method_call))) => {
                        self.collect_method_usage(method_call);
                        // Connect callbacks nested in an expression (assigned, or
                        // an argument of another call); bare-statement connects are
                        // handled in the statement match below.
                        self.event_callback_hint(method_call);
                    }
                    Either::Right(RValue::Call(call))
                    | Either::Right(RValue::Select(Select::Call(call))) => {
                        self.comparator_hint(call);
                    }
                    _ => {}
                }
                None
            });
            self.reserved.extend(globals);
            for function in functions {
                let mut function = function.lock();
                // Parameter heuristics need the whole function: a component (one
                // that renders an element) whose parameter is read as a record is
                // `props`; a parameter stored under an `onX`/`setX` field is that
                // callback.
                let renders_element =
                    uses_create_element(&function.body, &self.create_element_aliases);
                for param in &function.parameters {
                    self.props_param_hint(param, renders_element);
                    self.callback_hint(param);
                    self.usage_param_hint(param);
                }
                self.collect(&mut function.body, false);
            }

            match &*statement {
                // Bare-statement connects/sorts (`sig:Connect(fn)`,
                // `table.sort(t, fn)`): `post_traverse_values` only exposes a
                // statement's *nested* rvalues, never its own top-level call node,
                // so these must be matched at the statement level.
                Statement::MethodCall(method_call) => self.event_callback_hint(method_call),
                Statement::Call(call) => self.comparator_hint(call),
                Statement::Assign(assign) => {
                    for (right_index, rvalue) in assign.right.iter().enumerate() {
                        if right_index + 1 == assign.right.len()
                            && let RValue::Call(call) | RValue::Select(Select::Call(call)) = rvalue
                        {
                            self.apply_pcall_tuple_hints(assign, call, right_index);
                            self.apply_use_state_tuple_hints(assign, call, right_index);
                        }
                    }

                    for (index, lvalue) in assign.left.iter().enumerate() {
                        if let Some(local) = lvalue.as_local()
                            && let Some(rvalue) = assign.right.get(index)
                        {
                            if matches!(rvalue, RValue::Closure(_)) {
                                self.closure_locals.insert(local_ptr(local));
                                // A closure stored under an `onClose`/`setX` field
                                // takes that field's name.
                                self.callback_hint(local);
                            }
                            if let RValue::Table(table) = rvalue {
                                if is_root && assign.prefix {
                                    self.module_table_locals.insert(local_ptr(local));
                                }
                                if let Some(hint) = table_collection_hint(table) {
                                    self.set_hint(local, hint, 90);
                                }
                                self.children_or_result_hint(local);
                            }
                            // RHS-derived naming must not fire on a
                            // `conditional_expressions` diamond temp (`local v; if c then
                            // v = A else v = B end; use(v)`): naming it makes
                            // `is_generated_temp(v)` false and suppresses the collapse to
                            // `if c then A else B` (+lines). Such a temp is exactly the
                            // pass's gate `reads == 1 && writes == 3 && !captured`
                            // (conditional_expressions.rs:99); `is_collapse_candidate`
                            // mirrors it. Naming on a *single*-reassign (`local v; v = X`,
                            // writes == 2) or any multi-read local stays enabled, so the
                            // common hoisted-init names are preserved.
                            let collapse_candidate = self.is_collapse_candidate(local);
                            if !collapse_candidate {
                                if let Some(hint) = rvalue_hint(rvalue) {
                                    self.set_hint(local, hint, 60);
                                }
                            }
                            // §2.7 predicate/boolean naming fires only on DECLARATIONS
                            // (still prefix-gated) and additionally never on a collapse
                            // candidate.
                            if assign.prefix && !collapse_candidate {
                                // §2.7 Layer A: a predicate-call result reads as the
                                // predicate's subject (`local v = isFoo(x)` -> `foo`).
                                // Score 60 = rvalue_hint tier; `call_hint` returns
                                // nothing for a local-function callee, so this fills the
                                // empty slot without contending with a stronger hint.
                                if let Some(name) = self.predicate_call_hint(rvalue) {
                                    self.set_hint(local, name, 60);
                                }
                                // §2.7 Layer B: a boolean field/attribute test reads as
                                // the subject (`local v = X.Field == true` -> `field`).
                                // Score 58, just below the direct rvalue_hint tier.
                                if let Some(name) = boolean_compare_hint(rvalue) {
                                    self.set_hint(local, name, 58);
                                }
                            }
                            self.ref_hint(local, rvalue);
                            // Generic guarded-lookup children get parent-qualified
                            // (`plantedSeedsClient`) rather than colliding to
                            // `client`/`client2`.
                            if let Some((name, score)) =
                                self.guarded_lookup_qualified_hint(rvalue)
                            {
                                self.set_hint(local, name, score);
                            }
                        }
                    }
                }
                Statement::NumericFor(numeric_for) => {
                    self.set_hint_str(&numeric_for.counter, "i", 40);
                }
                Statement::GenericFor(generic_for) => {
                    let names = iterator_names(&generic_for.right);
                    // The element (second) variable can be named after the
                    // collection it iterates: `for index, crop in crops`.
                    let element_name = self.collection_element_name(&generic_for.right);
                    for (index, res_local) in generic_for.res_locals.iter().enumerate() {
                        if index == 1
                            && let Some(element_name) = &element_name
                        {
                            self.set_hint(res_local, element_name.clone(), 47);
                            continue;
                        }
                        let base = names
                            .as_ref()
                            .and_then(|n| n.get(index).copied())
                            .unwrap_or(if index == 0 { "k" } else { "v" });
                        match base {
                            "child" | "descendant" => {
                                self.set_context_hint_str(res_local, base, 45)
                            }
                            _ => self.set_hint_str(res_local, base, 30),
                        }
                    }
                }
                _ => {}
            }

            match &*statement {
                Statement::If(r#if) => {
                    self.collect(&mut r#if.then_block.lock(), false);
                    self.collect(&mut r#if.else_block.lock(), false);
                }
                Statement::While(r#while) => self.collect(&mut r#while.block.lock(), false),
                Statement::Repeat(repeat) => self.collect(&mut repeat.block.lock(), false),
                Statement::NumericFor(numeric_for) => {
                    self.collect(&mut numeric_for.block.lock(), false)
                }
                Statement::GenericFor(generic_for) => {
                    self.collect(&mut generic_for.block.lock(), false)
                }
                _ => {}
            }
        }

        if is_root
            && let Some(module_hint) = self.module_hint.clone()
            && let Some(Statement::Return(ret)) = block.0.last()
            && let [RValue::Local(local)] = ret.values.as_slice()
            && self.module_table_locals.contains(&local_ptr(local))
        {
            self.set_hint(local, module_hint, 100);
        }
    }

    /// Second pass: assign names. Outer/earlier locals are named before the
    /// locals of nested closures so they get the shorter, lower-numbered names.
    ///
    /// Names are reserved lexically: a `Block`'s prefix-assign locals stay
    /// reserved for the whole block (and its nested scopes), then are released so
    /// sibling blocks may reuse them; a `for`'s loop variables and a closure's
    /// parameters are reserved only for their own body. Enclosing-scope names
    /// remain reserved while naming nested scopes, so an inner local can never
    /// collide with a still-visible outer local (including a captured upvalue).
    fn apply(&mut self, block: &mut Block) {
        // Names reserved by prefix-assign locals declared directly in this block.
        // They remain visible until the end of the block, so release them last.
        let mut block_scope: Vec<String> = Vec::new();
        for statement in &mut block.0 {
            // Name the locals this statement declares directly into the block
            // scope BEFORE recursing into the statement's own nested scopes, so
            // the earliest declaration keeps the un-suffixed name.
            if let Statement::Assign(assign) = &*statement
                && assign.prefix
            {
                for lvalue in &assign.left {
                    if let Some(local) = lvalue.as_local() {
                        self.name_one(local, "v", &mut block_scope);
                    }
                }
            }

            // Closures appearing anywhere in this statement: their parameters are
            // scoped to the closure body only, so name them and apply the body in
            // a fresh scope, then release it for sibling closures to reuse.
            let mut functions = Vec::new();
            statement.post_traverse_values(&mut |value| -> Option<()> {
                if let Either::Right(RValue::Closure(closure)) = value {
                    functions.push(closure.function.clone());
                }
                None
            });
            for function in functions {
                let mut function = function.lock();
                let mut param_scope: Vec<String> = Vec::new();
                for param in &function.parameters {
                    self.name_one(param, "p", &mut param_scope);
                }
                self.apply(&mut function.body);
                self.release(param_scope);
            }

            // Nested blocks. A `for`'s loop variables are scoped to its body, so
            // name them into a fresh scope, apply the body, then release them so
            // sibling loops reuse `i`/`k`/`v`.
            match &*statement {
                Statement::If(r#if) => {
                    self.apply(&mut r#if.then_block.lock());
                    self.apply(&mut r#if.else_block.lock());
                }
                Statement::While(r#while) => self.apply(&mut r#while.block.lock()),
                Statement::Repeat(repeat) => self.apply(&mut repeat.block.lock()),
                Statement::NumericFor(numeric_for) => {
                    let mut loop_scope: Vec<String> = Vec::new();
                    self.name_one(&numeric_for.counter, "v", &mut loop_scope);
                    self.apply(&mut numeric_for.block.lock());
                    self.release(loop_scope);
                }
                Statement::GenericFor(generic_for) => {
                    let mut loop_scope: Vec<String> = Vec::new();
                    for res_local in &generic_for.res_locals {
                        self.name_one(res_local, "v", &mut loop_scope);
                    }
                    self.apply(&mut generic_for.block.lock());
                    self.release(loop_scope);
                }
                _ => {}
            }
        }
        self.release(block_scope);
    }
}

fn current_name(local: &RcLocal) -> Option<String> {
    local.0 .0.lock().0.clone().filter(|name| name != "_")
}

fn shadow_safe_base(name: &str) -> String {
    let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
    // Collapse a *generated default* name to its prefix when re-suffixing to
    // avoid a shadow (`v12` -> `v`, `k3` -> `k`, `i2` -> `i`). Every generated
    // prefix is a single letter (`v`/`p`/`k`/`i`/`a`/`b`), so a single-char stem
    // is the reliable tell. A genuine semantic name that merely ends in a digit
    // (`color3`, `udim2`, `vector2` from the Roblox constructors) has a
    // multi-char stem and must keep its digits, or it would be re-suffixed off a
    // different, misleading stem (`color3` -> `color`/`color2`). A `== 1` test
    // (not `<= 1`) keeps an all-digit name — which `sanitize` never actually
    // emits — mapping to itself rather than to the empty string.
    if base.len() == 1 {
        base.to_string()
    } else {
        name.to_string()
    }
}

fn unique_visible_name(base: &str, visible: &FxHashMap<String, usize>) -> String {
    if !visible.contains_key(base) {
        return base.to_string();
    }
    let mut counter = 2;
    loop {
        let candidate = format!("{}{}", base, counter);
        if !visible.contains_key(&candidate) {
            return candidate;
        }
        counter += 1;
    }
}

fn reserve_without_shadow(local: &RcLocal, visible: &mut FxHashMap<String, usize>) {
    let ptr = local_ptr(local);
    let Some(mut name) = current_name(local) else {
        return;
    };

    if visible.get(&name).is_some_and(|&existing| existing != ptr) {
        let base = shadow_safe_base(&name);
        let mut counter = 2;
        loop {
            let candidate = format!("{}{}", base, counter);
            if !visible.contains_key(&candidate) {
                local.0 .0.lock().0 = Some(candidate.clone());
                name = candidate;
                break;
            }
            counter += 1;
        }
    }

    visible.insert(name, ptr);
}

fn split_reused_loop_local(
    local: &mut RcLocal,
    body: &mut Block,
    visible: &FxHashMap<String, usize>,
) {
    let ptr = local_ptr(local);
    if !visible.values().any(|&existing| existing == ptr) {
        return;
    }

    let base = current_name(local)
        .map(|name| shadow_safe_base(&name))
        .unwrap_or_else(|| "v".to_string());
    let name = unique_visible_name(&base, visible);
    let new_local = RcLocal::new(Local::new(Some(name)));
    let mut map = std::collections::HashMap::new();
    map.insert(local.clone(), new_local.clone());
    crate::replace_locals::replace_locals(body, &map);
    *local = new_local;
}

fn avoid_shadowing_in_function(function: &mut crate::Function, visible: FxHashMap<String, usize>) {
    let mut visible = visible;
    for parameter in &function.parameters {
        reserve_without_shadow(parameter, &mut visible);
    }
    avoid_shadowing(&mut function.body, visible);
}

fn avoid_shadowing(block: &mut Block, mut visible: FxHashMap<String, usize>) {
    for statement in &mut block.0 {
        if let Statement::Assign(assign) = &*statement
            && assign.prefix
        {
            for lvalue in &assign.left {
                if let Some(local) = lvalue.as_local() {
                    reserve_without_shadow(local, &mut visible);
                }
            }
        }

        let mut functions = Vec::new();
        statement.post_traverse_values(&mut |value| -> Option<()> {
            if let Either::Right(RValue::Closure(closure)) = value {
                functions.push(closure.function.clone());
            }
            None
        });
        for function in functions {
            avoid_shadowing_in_function(&mut function.lock(), visible.clone());
        }

        match statement {
            Statement::If(r#if) => {
                avoid_shadowing(&mut r#if.then_block.lock(), visible.clone());
                avoid_shadowing(&mut r#if.else_block.lock(), visible.clone());
            }
            Statement::While(r#while) => {
                avoid_shadowing(&mut r#while.block.lock(), visible.clone())
            }
            Statement::Repeat(repeat) => avoid_shadowing(&mut repeat.block.lock(), visible.clone()),
            Statement::NumericFor(numeric_for) => {
                let mut loop_visible = visible.clone();
                let mut body = numeric_for.block.lock();
                split_reused_loop_local(&mut numeric_for.counter, &mut body, &loop_visible);
                reserve_without_shadow(&numeric_for.counter, &mut loop_visible);
                avoid_shadowing(&mut body, loop_visible);
            }
            Statement::GenericFor(generic_for) => {
                let mut loop_visible = visible.clone();
                let mut body = generic_for.block.lock();
                for res_local in &mut generic_for.res_locals {
                    split_reused_loop_local(res_local, &mut body, &loop_visible);
                    reserve_without_shadow(res_local, &mut loop_visible);
                }
                avoid_shadowing(&mut body, loop_visible);
            }
            _ => {}
        }
    }
}

pub fn name_locals(block: &mut Block, rename: bool) {
    name_locals_with_script_name(block, rename, None);
}

pub fn name_locals_with_script_name(block: &mut Block, rename: bool, script_name: Option<&str>) {
    // Gather, before naming, the whole-tree facts the scoring heuristics need:
    // which locals alias `createElement`, then per-local usage.
    let mut create_element_aliases = FxHashSet::default();
    collect_create_element_aliases(block, &mut create_element_aliases);
    let mut usage = FxHashMap::default();
    gather_usage(block, false, &create_element_aliases, &mut usage);
    // Read/write/capture counts via the same routine the elimination passes use,
    // so `is_collapse_candidate` matches their gate exactly. Re-key by `local_ptr`
    // and DROP the `RcLocal` keys (the `into_iter` consumes them) so no strong Arc
    // clone outlives this line — otherwise every local's `Arc::count` would be
    // inflated and `name_one`'s unused-local `_` detection would break.
    let counts: FxHashMap<usize, Usage> = collect_usage(block)
        .into_iter()
        .map(|(local, usage)| (local_ptr(&local), usage))
        .collect();
    let mut collapse_candidates = FxHashSet::default();
    collect_collapse_candidates(block, &mut collapse_candidates);

    let mut namer = Namer {
        rename,
        module_hint: script_name.and_then(script_module_hint),
        reserved: FxHashSet::default(),
        hints: FxHashMap::default(),
        context_hints: FxHashMap::default(),
        isa_families: FxHashMap::default(),
        module_table_locals: FxHashSet::default(),
        named: FxHashSet::default(),
        closure_locals: FxHashSet::default(),
        usage,
        create_element_aliases,
        counts,
        collapse_candidates,
    };
    namer.collect(block, true);
    namer.apply(block);
    if rename {
        avoid_shadowing(block, FxHashMap::default());
    }
}

#[cfg(test)]
mod tests {
    use super::{name_locals, name_locals_with_script_name};
    use crate::formatter::Formatter;
    use crate::{
        Assign, Binary, BinaryOperation, Block, Call, Closure, Function, GenericFor, Global, If,
        Index, LValue, Literal, MethodCall, NumericFor, RValue, RcLocal, Return, Statement, Table,
        Upvalue,
    };
    use by_address::ByAddress;
    use parking_lot::Mutex;
    use std::fmt;
    use triomphe::Arc;

    fn global(name: &str) -> RValue {
        RValue::Global(Global::from(name))
    }

    fn string(value: &str) -> RValue {
        RValue::Literal(Literal::String(value.as_bytes().to_vec()))
    }

    fn number(value: f64) -> RValue {
        RValue::Literal(Literal::Number(value))
    }

    fn boolean(value: bool) -> RValue {
        RValue::Literal(Literal::Boolean(value))
    }

    // `print(local)` — a use site so the local isn't treated as unused.
    fn use_local(local: &RcLocal) -> Statement {
        Statement::Call(Call::new(
            global("print"),
            vec![RValue::Local(local.clone())],
        ))
    }

    fn declare(local: &RcLocal, value: RValue) -> Statement {
        let mut assign = Assign::new(vec![LValue::Local(local.clone())], vec![value]);
        assign.prefix = true;
        assign.into()
    }

    fn name_of(local: &RcLocal) -> String {
        local.to_string()
    }

    fn named_local(name: &str) -> RcLocal {
        RcLocal::new(crate::Local::new(Some(name.to_string())))
    }

    #[test]
    fn meaningful_unique_non_shadowing_names() {
        let svc = RcLocal::default();
        let hum = RcLocal::default();
        let cfg = RcLocal::default();
        let counter = RcLocal::default();
        let callback = RcLocal::default();
        let param_a = RcLocal::default();
        let param_b = RcLocal::default();

        // local svc = game:GetService("Players")
        let svc_value = RValue::MethodCall(MethodCall::new(
            global("game"),
            "GetService".to_string(),
            vec![string("Players")],
        ));
        // local hum = char.Humanoid
        let hum_value = RValue::Index(Index::new(global("char"), string("Humanoid")));
        // local cfg = config   (config is also referenced as a global -> must not be shadowed)
        let cfg_value = global("config");

        // for counter = 1, 10 do print(counter) end
        let for_body = Block(vec![use_local(&counter)]);
        let numeric_for = NumericFor::new(
            number(1.0),
            number(10.0),
            number(1.0),
            counter.clone(),
            for_body,
        );

        // local callback = function(param_a, param_b) print(param_a) print(param_b) end
        let mut function = Function::default();
        function.parameters = vec![param_a.clone(), param_b.clone()];
        function.body = Block(vec![use_local(&param_a), use_local(&param_b)]);
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: Vec::new(),
        };

        let mut block = Block(vec![
            declare(&svc, svc_value),
            use_local(&svc),
            declare(&hum, hum_value),
            use_local(&hum),
            declare(&cfg, cfg_value),
            use_local(&cfg),
            Statement::NumericFor(numeric_for),
            declare(&callback, RValue::Closure(closure)),
            use_local(&callback),
        ]);

        name_locals(&mut block, true);

        // Hints produce readable names.
        assert_eq!(name_of(&svc), "Players");
        assert_eq!(name_of(&hum), "humanoid");
        assert_eq!(name_of(&counter), "i");
        assert_eq!(name_of(&callback), "fn");

        // The alias must not shadow the still-used `config` global.
        assert_eq!(name_of(&cfg), "config2");

        // Parameters get sequential, unique names.
        assert_eq!(name_of(&param_a), "p");
        assert_eq!(name_of(&param_b), "p2");

        // All assigned names are valid identifiers, unique, and never equal a
        // referenced global.
        let names = [
            name_of(&svc),
            name_of(&hum),
            name_of(&cfg),
            name_of(&counter),
            name_of(&callback),
            name_of(&param_a),
            name_of(&param_b),
        ];
        let used_globals = ["game", "char", "config", "print"];
        for name in &names {
            assert!(
                Formatter::<fmt::Formatter>::is_valid_name(name.as_bytes()),
                "{name} is not a valid identifier"
            );
            assert!(
                !used_globals.contains(&name.as_str()),
                "{name} shadows a referenced global"
            );
        }
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "names are not unique: {names:?}");

        // Block-level statements are separated by blank lines for readability.
        let output = block.to_string();
        assert!(
            output.contains("\n\n"),
            "expected blank lines around block statements:\n{output}"
        );
    }

    #[test]
    fn upvalues_and_generic_for() {
        let upvalue = RcLocal::default();
        let callback = RcLocal::default();
        let tbl = RcLocal::default();
        let key = RcLocal::default();
        let value = RcLocal::default();

        // local upvalue = state.Value
        let upvalue_decl = declare(
            &upvalue,
            RValue::Index(Index::new(global("state"), string("Value"))),
        );

        // local callback = function() print(upvalue) end   -- captures `upvalue`
        let mut function = Function::default();
        function.body = Block(vec![use_local(&upvalue)]);
        let closure = Closure {
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: vec![Upvalue::Ref(upvalue.clone())],
        };
        let callback_decl = declare(&callback, RValue::Closure(closure));

        // local tbl = data.Items
        let tbl_decl = declare(
            &tbl,
            RValue::Index(Index::new(global("data"), string("Items"))),
        );

        // for key, value in pairs(tbl) do print(key) print(value) end
        let for_body = Block(vec![use_local(&key), use_local(&value)]);
        let generic_for = GenericFor::new(
            vec![key.clone(), value.clone()],
            vec![RValue::Call(Call::new(
                global("pairs"),
                vec![RValue::Local(tbl.clone())],
            ))],
            for_body,
        );

        let mut block = Block(vec![
            upvalue_decl,
            callback_decl,
            use_local(&callback),
            tbl_decl,
            Statement::GenericFor(generic_for),
        ]);

        name_locals(&mut block, true);

        // The captured local is named once, from its field hint.
        assert_eq!(name_of(&upvalue), "value");
        // `pairs` iteration names the key `k`; the element variable is
        // singularized from the iterated collection `items` -> `item`.
        assert_eq!(name_of(&key), "k");
        assert_eq!(name_of(&value), "item");
        assert_eq!(name_of(&tbl), "items");

        // The closure body refers to the upvalue by the very same name.
        let output = block.to_string();
        assert!(
            output.contains("print(value)"),
            "closure should reference the captured local consistently:\n{output}"
        );
    }

    #[test]
    fn getter_method_hint() {
        let kids = RcLocal::default();
        let mouse = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &kids,
                RValue::MethodCall(MethodCall::new(
                    global("workspace"),
                    "GetChildren".to_string(),
                    vec![],
                )),
            ),
            use_local(&kids),
            declare(
                &mouse,
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(kids.clone()),
                    "GetMouse".to_string(),
                    vec![],
                )),
            ),
            use_local(&mouse),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&kids), "children");
        assert_eq!(name_of(&mouse), "mouse");
    }

    #[test]
    fn usage_context_names_module_target_folders_and_descendants() {
        let workspace = RcLocal::default();
        let module = RcLocal::default();
        let folders = RcLocal::default();
        let self_param = RcLocal::default();
        let folder_param = RcLocal::default();
        let index = RcLocal::default();
        let descendant = RcLocal::default();

        let workspace_decl = declare(
            &workspace,
            RValue::MethodCall(MethodCall::new(
                global("game"),
                "GetService".to_string(),
                vec![string("Workspace")],
            )),
        );

        let module_decl = declare(&module, RValue::Table(Table::default()));
        let folders_decl = declare(
            &folders,
            RValue::Table(Table(vec![
                (
                    None,
                    RValue::MethodCall(MethodCall::new(
                        RValue::Local(workspace.clone()),
                        "WaitForChild".to_string(),
                        vec![string("NPCS")],
                    )),
                ),
                (
                    None,
                    RValue::MethodCall(MethodCall::new(
                        RValue::Local(workspace.clone()),
                        "FindFirstChild".to_string(),
                        vec![string("Debris")],
                    )),
                ),
                (
                    None,
                    RValue::Index(Index::new(
                        RValue::Local(workspace.clone()),
                        string("Animals"),
                    )),
                ),
            ])),
        );

        let mut collision_assign = Assign::new(
            vec![LValue::Index(Index::new(
                RValue::Local(descendant.clone()),
                string("CollisionGroup"),
            ))],
            vec![string("Default")],
        );
        collision_assign.prefix = false;

        let loop_body = Block(vec![If::new(
            RValue::MethodCall(MethodCall::new(
                RValue::Local(descendant.clone()),
                "IsA".to_string(),
                vec![string("BasePart")],
            )),
            Block(vec![Statement::Assign(collision_assign)]),
            Block::default(),
        )
        .into()]);

        let generic_for = GenericFor::new(
            vec![index.clone(), descendant.clone()],
            vec![RValue::Call(Call::new(
                global("ipairs"),
                vec![RValue::MethodCall(MethodCall::new(
                    RValue::Local(folder_param.clone()),
                    "GetDescendants".to_string(),
                    vec![],
                ))],
            ))],
            loop_body,
        );

        let mut function = Function::default();
        function.parameters = vec![self_param.clone(), folder_param.clone()];
        function.body = Block(vec![Statement::GenericFor(generic_for)]);
        let method_assign = Statement::Assign(Assign::new(
            vec![LValue::Index(Index::new(
                RValue::Local(module.clone()),
                string("DisableCollision"),
            ))],
            vec![RValue::Closure(Closure {
                function: ByAddress(Arc::new(Mutex::new(function))),
                upvalues: Vec::new(),
            })],
        ));

        let mut block = Block(vec![
            workspace_decl,
            module_decl,
            folders_decl,
            method_assign,
            Statement::Return(crate::Return::new(vec![RValue::Local(module.clone())])),
        ]);

        name_locals_with_script_name(&mut block, true, Some("collision.client.luau"));

        assert_eq!(name_of(&workspace), "Workspace");
        assert_eq!(name_of(&module), "Collision");
        assert_eq!(name_of(&folders), "TargetFolders");
        assert_eq!(name_of(&folder_param), "folder");
        assert_eq!(name_of(&descendant), "part");
    }

    #[test]
    fn mixed_isa_get_descendants_uses_context_name() {
        let index = RcLocal::default();
        let descendant = RcLocal::default();

        let loop_body = Block(vec![
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(descendant.clone()),
                    "IsA".to_string(),
                    vec![string("Script")],
                )),
                Block(vec![use_local(&descendant)]),
                Block::default(),
            )
            .into(),
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(descendant.clone()),
                    "IsA".to_string(),
                    vec![string("LocalScript")],
                )),
                Block(vec![use_local(&descendant)]),
                Block::default(),
            )
            .into(),
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(descendant.clone()),
                    "IsA".to_string(),
                    vec![string("BasePart")],
                )),
                Block(vec![use_local(&descendant)]),
                Block::default(),
            )
            .into(),
        ]);

        let generic_for = GenericFor::new(
            vec![index.clone(), descendant.clone()],
            vec![RValue::MethodCall(MethodCall::new(
                global("model"),
                "GetDescendants".to_string(),
                vec![],
            ))],
            loop_body,
        );

        let mut block = Block(vec![Statement::GenericFor(generic_for)]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&descendant), "descendant");
    }

    #[test]
    fn mixed_isa_ipairs_get_descendants_uses_context_name() {
        let index = RcLocal::default();
        let descendant = RcLocal::default();

        let loop_body = Block(vec![
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(descendant.clone()),
                    "IsA".to_string(),
                    vec![string("Script")],
                )),
                Block(vec![use_local(&descendant)]),
                Block::default(),
            )
            .into(),
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(descendant.clone()),
                    "IsA".to_string(),
                    vec![string("BasePart")],
                )),
                Block(vec![use_local(&descendant)]),
                Block::default(),
            )
            .into(),
        ]);

        let generic_for = GenericFor::new(
            vec![index.clone(), descendant.clone()],
            vec![RValue::Call(Call::new(
                global("ipairs"),
                vec![RValue::MethodCall(MethodCall::new(
                    global("model"),
                    "GetDescendants".to_string(),
                    vec![],
                ))],
            ))],
            loop_body,
        );

        let mut block = Block(vec![Statement::GenericFor(generic_for)]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&descendant), "descendant");
    }

    #[test]
    fn script_and_local_script_isa_stays_script() {
        let index = RcLocal::default();
        let value = RcLocal::default();

        let loop_body = Block(vec![
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(value.clone()),
                    "IsA".to_string(),
                    vec![string("Script")],
                )),
                Block(vec![use_local(&value)]),
                Block::default(),
            )
            .into(),
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(value.clone()),
                    "IsA".to_string(),
                    vec![string("LocalScript")],
                )),
                Block(vec![use_local(&value)]),
                Block::default(),
            )
            .into(),
        ]);

        let generic_for = GenericFor::new(
            vec![index.clone(), value.clone()],
            vec![RValue::MethodCall(MethodCall::new(
                global("model"),
                "GetChildren".to_string(),
                vec![],
            ))],
            loop_body,
        );

        let mut block = Block(vec![Statement::GenericFor(generic_for)]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&value), "script");
    }

    #[test]
    fn module_script_mixed_with_script_stays_script() {
        let index = RcLocal::default();
        let value = RcLocal::default();

        let loop_body = Block(vec![
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(value.clone()),
                    "IsA".to_string(),
                    vec![string("ModuleScript")],
                )),
                Block(vec![use_local(&value)]),
                Block::default(),
            )
            .into(),
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(value.clone()),
                    "IsA".to_string(),
                    vec![string("Script")],
                )),
                Block(vec![use_local(&value)]),
                Block::default(),
            )
            .into(),
        ]);

        let generic_for = GenericFor::new(
            vec![index.clone(), value.clone()],
            vec![RValue::MethodCall(MethodCall::new(
                global("model"),
                "GetChildren".to_string(),
                vec![],
            ))],
            loop_body,
        );

        let mut block = Block(vec![Statement::GenericFor(generic_for)]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&value), "script");
    }

    #[test]
    fn mixed_effect_isa_keeps_specific_effect_name() {
        let index = RcLocal::default();
        let value = RcLocal::default();

        let loop_body = Block(vec![
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(value.clone()),
                    "IsA".to_string(),
                    vec![string("ParticleEmitter")],
                )),
                Block(vec![use_local(&value)]),
                Block::default(),
            )
            .into(),
            If::new(
                RValue::MethodCall(MethodCall::new(
                    RValue::Local(value.clone()),
                    "IsA".to_string(),
                    vec![string("Beam")],
                )),
                Block(vec![use_local(&value)]),
                Block::default(),
            )
            .into(),
        ]);

        let generic_for = GenericFor::new(
            vec![index.clone(), value.clone()],
            vec![RValue::MethodCall(MethodCall::new(
                global("model"),
                "GetChildren".to_string(),
                vec![],
            ))],
            loop_body,
        );

        let mut block = Block(vec![Statement::GenericFor(generic_for)]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&value), "emitter");
    }

    #[test]
    fn pcall_tuple_gets_success_and_result_names() {
        let success = RcLocal::default();
        let result = RcLocal::default();
        let mut function = Function::default();
        function.body = Block(vec![Statement::Return(crate::Return::new(vec![number(
            1.0,
        )]))]);
        let closure = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: Vec::new(),
        });

        let mut pcall_assign = Assign::new(
            vec![
                LValue::Local(success.clone()),
                LValue::Local(result.clone()),
            ],
            vec![RValue::Call(Call::new(global("pcall"), vec![closure]))],
        );
        pcall_assign.prefix = true;
        let mut block = Block(vec![
            Statement::Assign(pcall_assign),
            use_local(&success),
            use_local(&result),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&success), "success");
        assert_eq!(name_of(&result), "result");
    }

    #[test]
    fn pcall_tuple_hints_only_expand_from_last_rhs() {
        let first_success = RcLocal::default();
        let fallback = RcLocal::default();
        let prefix_value = RcLocal::default();
        let later_success = RcLocal::default();
        let later_result = RcLocal::default();
        let mut function = Function::default();
        function.body = Block(vec![Statement::Return(crate::Return::new(vec![number(
            1.0,
        )]))]);
        let closure = || {
            RValue::Closure(Closure {
                function: ByAddress(Arc::new(Mutex::new(function.clone()))),
                upvalues: Vec::new(),
            })
        };

        let mut non_expanding = Assign::new(
            vec![
                LValue::Local(first_success.clone()),
                LValue::Local(fallback.clone()),
            ],
            vec![
                RValue::Call(Call::new(global("pcall"), vec![closure()])),
                boolean(false),
            ],
        );
        non_expanding.prefix = true;

        let mut expanding = Assign::new(
            vec![
                LValue::Local(prefix_value.clone()),
                LValue::Local(later_success.clone()),
                LValue::Local(later_result.clone()),
            ],
            vec![
                boolean(false),
                RValue::Call(Call::new(global("pcall"), vec![closure()])),
            ],
        );
        expanding.prefix = true;

        let mut block = Block(vec![
            Statement::Assign(non_expanding),
            use_local(&first_success),
            use_local(&fallback),
            Statement::Assign(expanding),
            use_local(&prefix_value),
            use_local(&later_success),
            use_local(&later_result),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&first_success), "v");
        assert_eq!(name_of(&fallback), "v2");
        assert_eq!(name_of(&later_success), "success");
        assert_eq!(name_of(&later_result), "result");
    }

    #[test]
    fn use_state_tuple_gets_state_and_setter_names() {
        let state = RcLocal::default();
        let setter = RcLocal::default();
        let mut use_state_assign = Assign::new(
            vec![LValue::Local(state.clone()), LValue::Local(setter.clone())],
            vec![RValue::Call(Call::new(
                RValue::Index(Index::new(global("React"), string("useState"))),
                vec![boolean(false)],
            ))],
        );
        use_state_assign.prefix = true;
        let mut block = Block(vec![
            Statement::Assign(use_state_assign),
            use_local(&state),
            Statement::Call(Call::new(
                RValue::Local(setter.clone()),
                vec![boolean(true)],
            )),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&state), "state");
        assert_eq!(name_of(&setter), "setState");
    }

    #[test]
    fn module_script_name_does_not_override_non_declaration_table_assignment() {
        let module = RcLocal::default();
        let mut module_decl = Assign::new(
            vec![LValue::Local(module.clone())],
            vec![RValue::Call(Call::new(
                global("require"),
                vec![RValue::Index(Index::new(global("script"), string("Foo")))],
            ))],
        );
        module_decl.prefix = true;

        let mut reset_assign = Assign::new(
            vec![LValue::Local(module.clone())],
            vec![RValue::Table(Table::default())],
        );
        reset_assign.prefix = false;

        let mut block = Block(vec![
            Statement::Assign(module_decl),
            Statement::Assign(reset_assign),
            Statement::Return(crate::Return::new(vec![RValue::Local(module.clone())])),
        ]);

        name_locals_with_script_name(&mut block, true, Some("Collision.luau"));

        assert_eq!(name_of(&module), "foo");
    }

    #[test]
    fn module_script_name_uses_dot_path_parent_for_init_modules() {
        let module = RcLocal::default();
        let mut block = Block(vec![
            declare(&module, RValue::Table(Table::default())),
            use_local(&module),
            Statement::Return(crate::Return::new(vec![RValue::Local(module.clone())])),
        ]);

        name_locals_with_script_name(
            &mut block,
            true,
            Some("ReplicatedStorage.Client.UI.Inventory.init"),
        );

        assert_eq!(name_of(&module), "Inventory");
    }

    // local part = Instance.new("Part") — a hint-bearing declaration whose hint
    // resolves to "part".
    fn declare_part(local: &RcLocal) -> Statement {
        declare(
            local,
            RValue::Call(Call::new(
                RValue::Index(Index::new(global("Instance"), string("new"))),
                vec![string("Part")],
            )),
        )
    }

    // Two SIBLING closures, each declaring its own `part`. The scopes are
    // disjoint, so the second must NOT be suffixed — both end up `part`.
    #[test]
    fn sibling_closures_reuse_base_name() {
        let part_a = RcLocal::default();
        let part_b = RcLocal::default();

        let mut fn_a = Function::default();
        fn_a.body = Block(vec![declare_part(&part_a), use_local(&part_a)]);
        let closure_a = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(fn_a))),
            upvalues: Vec::new(),
        });

        let mut fn_b = Function::default();
        fn_b.body = Block(vec![declare_part(&part_b), use_local(&part_b)]);
        let closure_b = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(fn_b))),
            upvalues: Vec::new(),
        });

        // Two anonymous closures invoked as statements (sibling, non-overlapping).
        let mut block = Block(vec![
            Statement::Call(Call::new(closure_a, vec![])),
            Statement::Call(Call::new(closure_b, vec![])),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&part_a), "part");
        assert_eq!(
            name_of(&part_b),
            "part",
            "sibling closure should reuse `part`"
        );
    }

    // Two SIBLING numeric `for` loops both name their counter `i` — the second
    // loop's variable is out of scope of the first, so no `i2`.
    #[test]
    fn sibling_for_loops_reuse_counter() {
        let i_a = RcLocal::default();
        let i_b = RcLocal::default();

        let for_a = Statement::NumericFor(NumericFor::new(
            number(1.0),
            number(10.0),
            number(1.0),
            i_a.clone(),
            Block(vec![use_local(&i_a)]),
        ));
        let for_b = Statement::NumericFor(NumericFor::new(
            number(1.0),
            number(10.0),
            number(1.0),
            i_b.clone(),
            Block(vec![use_local(&i_b)]),
        ));

        let mut block = Block(vec![for_a, for_b]);
        name_locals(&mut block, true);

        assert_eq!(name_of(&i_a), "i");
        assert_eq!(name_of(&i_b), "i", "sibling for loop should reuse `i`");
    }

    #[test]
    fn already_named_branch_local_is_reserved_for_nested_scopes() {
        let shared = RcLocal::default();
        let key = RcLocal::default();
        let value = RcLocal::default();

        let then_block = Block(vec![declare(&shared, number(1.0)), use_local(&shared)]);
        let else_block = Block(vec![
            declare(&shared, number(2.0)),
            GenericFor::new(
                vec![key.clone(), value.clone()],
                vec![global("pairs")],
                Block(vec![use_local(&value)]),
            )
            .into(),
            use_local(&shared),
        ]);
        let mut block = Block(vec![If::new(global("cond"), then_block, else_block).into()]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&shared), "v");
        assert_eq!(name_of(&key), "k");
        assert_eq!(
            name_of(&value),
            "v2",
            "loop value must not shadow the already-named branch local"
        );
    }

    #[test]
    fn rename_false_preserves_existing_shadowing_names() {
        let outer = named_local("value");
        let inner = named_local("value");

        let mut function = Function::default();
        function.body = Block(vec![declare(&inner, number(2.0)), use_local(&inner)]);
        let closure = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: vec![Upvalue::Ref(outer.clone())],
        });

        let mut block = Block(vec![
            declare(&outer, number(1.0)),
            Statement::Call(Call::new(closure, vec![])),
            use_local(&outer),
        ]);

        name_locals(&mut block, false);

        assert_eq!(name_of(&outer), "value");
        assert_eq!(
            name_of(&inner),
            "value",
            "rename=false must not rewrite existing shadowing names"
        );
    }

    // A nested local that COEXISTS with a still-visible outer local of the same
    // hint MUST be suffixed — the invariant that simultaneously-visible locals
    // never share a name. The outer `part` is declared in the block, captured by
    // a closure that also declares its own `part` and is then used AFTER the
    // closure, so both are live at once.
    #[test]
    fn coexisting_locals_stay_distinct() {
        let outer = RcLocal::default();
        let inner = RcLocal::default();

        // local outer = Instance.new("Part")
        let outer_decl = declare_part(&outer);

        // closure that captures `outer` and declares its own `inner` part:
        //   function() print(outer) local inner = Instance.new("Part") print(inner) end
        let mut function = Function::default();
        function.body = Block(vec![
            use_local(&outer),
            declare_part(&inner),
            use_local(&inner),
        ]);
        let closure = RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: vec![Upvalue::Ref(outer.clone())],
        });

        let mut block = Block(vec![
            outer_decl,
            Statement::Call(Call::new(closure, vec![])),
            // outer is still used here, so it stays in scope across the closure.
            use_local(&outer),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&outer), "part");
        assert_eq!(
            name_of(&inner),
            "part2",
            "inner local coexisting with a visible outer `part` must be suffixed"
        );
        assert_ne!(name_of(&outer), name_of(&inner));
    }

    // Prints a representative decompilation. Run with:
    //   cargo test -p ast demo_output -- --nocapture
    #[test]
    fn demo_output() {
        let players = RcLocal::default();
        let part = RcLocal::default();
        let handler = RcLocal::default();
        let i = RcLocal::default();
        let hit = RcLocal::default();

        // local players = game:GetService("Players")
        let s1 = declare(
            &players,
            RValue::MethodCall(MethodCall::new(
                global("game"),
                "GetService".to_string(),
                vec![string("Players")],
            )),
        );
        // local part = Instance.new("Part")
        let s2 = declare(
            &part,
            RValue::Call(Call::new(
                RValue::Index(Index::new(global("Instance"), string("new"))),
                vec![string("Part")],
            )),
        );
        // part.Name = "Greeting"
        let mut name_assign = Assign::new(
            vec![LValue::Index(Index::new(
                RValue::Local(part.clone()),
                string("Name"),
            ))],
            vec![string("Greeting")],
        );
        name_assign.prefix = false;
        let s3 = Statement::Assign(name_assign);
        // for i = 1, 5 do print(i) end
        let s4 = Statement::NumericFor(NumericFor::new(
            number(1.0),
            number(5.0),
            number(1.0),
            i.clone(),
            Block(vec![use_local(&i)]),
        ));
        // local handler = function(hit) print(hit) end
        let mut function = Function::default();
        function.parameters = vec![hit.clone()];
        function.body = Block(vec![use_local(&hit)]);
        let s5 = declare(
            &handler,
            RValue::Closure(Closure {
                function: ByAddress(Arc::new(Mutex::new(function))),
                upvalues: Vec::new(),
            }),
        );

        let mut block = Block(vec![
            s1,
            s2,
            s3,
            s4,
            s5,
            use_local(&handler),
            use_local(&players),
        ]);
        name_locals(&mut block, true);
        println!("\n===== DECOMPILED OUTPUT =====\n{block}\n=============================");
    }

    // ---- Heuristics: props / children / result / ref / callback / iterator ----

    fn field(local: &RcLocal, key: &str) -> RValue {
        RValue::Index(Index::new(RValue::Local(local.clone()), string(key)))
    }

    /// `react.createElement(args...)`.
    fn create_element(args: Vec<RValue>) -> RValue {
        RValue::Call(Call::new(
            RValue::Index(Index::new(global("react"), string("createElement"))),
            args,
        ))
    }

    fn closure_of(function: Function) -> RValue {
        RValue::Closure(Closure {
            function: ByAddress(Arc::new(Mutex::new(function))),
            upvalues: vec![],
        })
    }

    fn ret(values: Vec<RValue>) -> Statement {
        Statement::Return(Return::new(values))
    }

    fn keyed_assign(table: &RcLocal, key: RValue, value: RValue) -> Statement {
        Assign::new(
            vec![LValue::Index(Index::new(RValue::Local(table.clone()), key))],
            vec![value],
        )
        .into()
    }

    /// A component (returns `createElement`) whose sole parameter is read as a
    /// record of >=3 distinct named fields becomes `props`.
    #[test]
    fn props_param_named_from_record_fields() {
        let p = RcLocal::default();
        let (a, b, c) = (RcLocal::default(), RcLocal::default(), RcLocal::default());
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            declare(&a, field(&p, "visible")),
            declare(&b, field(&p, "currentTabId")),
            declare(&c, field(&p, "onClose")),
            ret(vec![create_element(vec![
                string("Frame"),
                RValue::Table(Table::default()),
            ])]),
        ]);
        let comp = RcLocal::default();
        let mut block = Block(vec![declare(&comp, closure_of(function)), use_local(&comp)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "props");
    }

    /// Two fields is not enough: a Vector-like `p.X`, `p.Y` must stay `p`.
    #[test]
    fn props_param_refused_with_too_few_fields() {
        let p = RcLocal::default();
        let (a, b) = (RcLocal::default(), RcLocal::default());
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            declare(&a, field(&p, "x")),
            declare(&b, field(&p, "y")),
            ret(vec![create_element(vec![string("Frame")])]),
        ]);
        let comp = RcLocal::default();
        let mut block = Block(vec![declare(&comp, closure_of(function)), use_local(&comp)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "p");
    }

    /// A parameter that is invoked (`p()`) is a callback, not a record.
    #[test]
    fn props_param_refused_when_invoked() {
        let p = RcLocal::default();
        let (a, b, c) = (RcLocal::default(), RcLocal::default(), RcLocal::default());
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            declare(&a, field(&p, "x")),
            declare(&b, field(&p, "y")),
            declare(&c, field(&p, "z")),
            Statement::Call(Call::new(RValue::Local(p.clone()), vec![])),
            ret(vec![create_element(vec![string("Frame")])]),
        ]);
        let comp = RcLocal::default();
        let mut block = Block(vec![declare(&comp, closure_of(function)), use_local(&comp)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "p");
    }

    /// A numerically-indexed parameter (`p[1]`) is an array, not a record.
    #[test]
    fn props_param_refused_when_numeric_indexed() {
        let p = RcLocal::default();
        let (a, b, c, d) = (
            RcLocal::default(),
            RcLocal::default(),
            RcLocal::default(),
            RcLocal::default(),
        );
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            declare(&a, field(&p, "x")),
            declare(&b, field(&p, "y")),
            declare(&c, field(&p, "z")),
            declare(
                &d,
                RValue::Index(Index::new(RValue::Local(p.clone()), number(1.0))),
            ),
            ret(vec![create_element(vec![string("Frame")])]),
        ]);
        let comp = RcLocal::default();
        let mut block = Block(vec![declare(&comp, closure_of(function)), use_local(&comp)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "p");
    }

    /// Without a `createElement` render the function is not a component, so a
    /// record-shaped parameter still stays `p`.
    #[test]
    fn props_param_refused_for_non_component() {
        let p = RcLocal::default();
        let (a, b, c) = (RcLocal::default(), RcLocal::default(), RcLocal::default());
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            declare(&a, field(&p, "x")),
            declare(&b, field(&p, "y")),
            declare(&c, field(&p, "z")),
            ret(vec![boolean(true)]),
        ]);
        let comp = RcLocal::default();
        let mut block = Block(vec![declare(&comp, closure_of(function)), use_local(&comp)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "p");
    }

    /// `local t = {}` filled in a loop with `createElement` is a `children` map.
    #[test]
    fn children_accumulator_named_from_create_element_fill() {
        let children = RcLocal::default();
        let (k, v, list) = (RcLocal::default(), RcLocal::default(), RcLocal::default());
        let loop_body = Block(vec![keyed_assign(
            &children,
            string("Paragraph_1"),
            create_element(vec![string("TextLabel"), RValue::Table(Table::default())]),
        )]);
        let generic_for = GenericFor::new(
            vec![k.clone(), v.clone()],
            vec![RValue::Call(Call::new(
                global("pairs"),
                vec![RValue::Local(list.clone())],
            ))],
            loop_body,
        );
        let mut block = Block(vec![
            declare(&children, RValue::Table(Table::default())),
            Statement::GenericFor(generic_for),
            ret(vec![RValue::Local(children.clone())]),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&children), "children");
    }

    /// `local t = {}` filled in a loop with plain data and returned is `result`.
    #[test]
    fn result_accumulator_named_when_filled_in_loop_and_returned() {
        let out = RcLocal::default();
        let counter = RcLocal::default();
        let loop_body = Block(vec![keyed_assign(
            &out,
            RValue::Local(counter.clone()),
            boolean(true),
        )]);
        let numeric_for = NumericFor::new(
            number(1.0),
            number(10.0),
            number(1.0),
            counter.clone(),
            loop_body,
        );
        let mut block = Block(vec![
            declare(&out, RValue::Table(Table::default())),
            Statement::NumericFor(numeric_for),
            ret(vec![RValue::Local(out.clone())]),
        ]);
        name_locals(&mut block, true);
        // The accumulator is `result`; the loop counter keeps `i` (score 40 > 35).
        assert_eq!(name_of(&out), "result");
        assert_eq!(name_of(&counter), "i");
    }

    /// `react.useRef(...)` reads as `ref`.
    #[test]
    fn ref_named_from_use_ref_call() {
        let r = RcLocal::default();
        let use_ref = RValue::Call(Call::new(
            RValue::Index(Index::new(global("react"), string("useRef"))),
            vec![number(0.0)],
        ));
        let mut block = Block(vec![declare(&r, use_ref), use_local(&r)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&r), "ref");
    }

    /// A closure stored under an `onClose` field takes that name; one under a
    /// non-callback field (`layout`) does not.
    #[test]
    fn callback_named_from_event_field() {
        let on_close = RcLocal::default();
        let layout = RcLocal::default();
        let handlers = RcLocal::default();
        let table = Table(vec![
            (Some(string("onClose")), RValue::Local(on_close.clone())),
            (Some(string("layout")), RValue::Local(layout.clone())),
        ]);
        let mut block = Block(vec![
            declare(&on_close, closure_of(Function::default())),
            declare(&layout, closure_of(Function::default())),
            declare(&handlers, RValue::Table(table)),
            use_local(&handlers),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&on_close), "onClose");
        assert_eq!(name_of(&layout), "fn");
    }

    /// The element variable is singularized from the iterated collection name.
    #[test]
    fn iterator_element_singularized_from_collection() {
        let crops = RcLocal::default();
        let (index, crop) = (RcLocal::default(), RcLocal::default());
        let for_body = Block(vec![use_local(&index), use_local(&crop)]);
        let generic_for = GenericFor::new(
            vec![index.clone(), crop.clone()],
            vec![RValue::Local(crops.clone())],
            for_body,
        );
        let mut block = Block(vec![
            declare(
                &crops,
                RValue::Index(Index::new(global("data"), string("Crops"))),
            ),
            Statement::GenericFor(generic_for),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&crops), "crops");
        assert_eq!(name_of(&crop), "crop");
    }

    /// A non-plural collection name (`status`) is not singularized into a
    /// non-word; the element variable falls back to the default.
    #[test]
    fn iterator_element_refuses_non_plural_collection() {
        let status = RcLocal::default();
        let (k, v) = (RcLocal::default(), RcLocal::default());
        let for_body = Block(vec![use_local(&k), use_local(&v)]);
        let generic_for = GenericFor::new(
            vec![k.clone(), v.clone()],
            vec![RValue::Local(status.clone())],
            for_body,
        );
        let mut block = Block(vec![
            declare(
                &status,
                RValue::Index(Index::new(global("data"), string("Status"))),
            ),
            Statement::GenericFor(generic_for),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&status), "status");
        assert_eq!(name_of(&v), "v");
    }

    /// A Latin irregular plural (`indices`) must not become a non-word (`indice`);
    /// the element variable falls back to the default.
    #[test]
    fn iterator_element_refuses_latin_irregular() {
        let indices = RcLocal::default();
        let (k, v) = (RcLocal::default(), RcLocal::default());
        let for_body = Block(vec![use_local(&k), use_local(&v)]);
        let generic_for = GenericFor::new(
            vec![k.clone(), v.clone()],
            vec![RValue::Local(indices.clone())],
            for_body,
        );
        let mut block = Block(vec![
            declare(
                &indices,
                RValue::Index(Index::new(global("mesh"), string("Indices"))),
            ),
            Statement::GenericFor(generic_for),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&indices), "indices");
        assert_eq!(name_of(&v), "v");
    }

    /// A config/props table that merely holds ONE nested element among scalar
    /// fields (no loop) must NOT be mislabeled `children`.
    #[test]
    fn children_refused_for_single_inline_element() {
        let t = RcLocal::default();
        let mut block = Block(vec![
            declare(&t, RValue::Table(Table::default())),
            keyed_assign(&t, string("Padding"), number(8.0)),
            keyed_assign(
                &t,
                string("Icon"),
                create_element(vec![string("ImageLabel"), RValue::Table(Table::default())]),
            ),
            ret(vec![RValue::Local(t.clone())]),
        ]);
        name_locals(&mut block, true);
        assert_ne!(name_of(&t), "children");
    }

    /// `table.insert(children, createElement(...))` in a loop is an array-style
    /// children map -> `children` (not `result`).
    #[test]
    fn children_from_table_insert_create_element_in_loop() {
        let children = RcLocal::default();
        let (k, v, list) = (RcLocal::default(), RcLocal::default(), RcLocal::default());
        let insert = Statement::Call(Call::new(
            RValue::Index(Index::new(global("table"), string("insert"))),
            vec![
                RValue::Local(children.clone()),
                create_element(vec![string("TextLabel"), RValue::Table(Table::default())]),
            ],
        ));
        let generic_for = GenericFor::new(
            vec![k.clone(), v.clone()],
            vec![RValue::Call(Call::new(
                global("pairs"),
                vec![RValue::Local(list.clone())],
            ))],
            Block(vec![insert]),
        );
        let mut block = Block(vec![
            declare(&children, RValue::Table(Table::default())),
            Statement::GenericFor(generic_for),
            ret(vec![RValue::Local(children.clone())]),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&children), "children");
    }

    /// A `setX` field also names a stored closure (the `set` branch of the
    /// callback key check).
    #[test]
    fn callback_named_from_setter_field() {
        let setter = RcLocal::default();
        let handlers = RcLocal::default();
        let table = Table(vec![(Some(string("setVisible")), RValue::Local(setter.clone()))]);
        let mut block = Block(vec![
            declare(&setter, closure_of(Function::default())),
            declare(&handlers, RValue::Table(table)),
            use_local(&handlers),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&setter), "setVisible");
    }

    // `receiver and receiver:FindFirstChild("Child")` — the nil-guarded lookup.
    fn guarded_find(receiver: RValue, child: &str) -> RValue {
        RValue::Binary(Binary::new(
            receiver.clone(),
            RValue::MethodCall(MethodCall::new(
                receiver,
                "FindFirstChild".to_string(),
                vec![string(child)],
            )),
            BinaryOperation::And,
        ))
    }

    fn find_first_child(receiver: RValue, child: &str) -> RValue {
        RValue::MethodCall(MethodCall::new(
            receiver,
            "FindFirstChild".to_string(),
            vec![string(child)],
        ))
    }

    /// The flagship case (ClientPerformanceDebug): guarded lookups are named after
    /// the looked-up child, and the two colliding generic `Client` children are
    /// parent-qualified instead of becoming `client`/`client2`.
    #[test]
    fn guarded_lookup_names_and_qualifies_generic_children() {
        let world = RcLocal::default();
        let seeds = RcLocal::default();
        let pots = RcLocal::default();
        let seeds_client = RcLocal::default();
        let pots_client = RcLocal::default();

        let mut block = Block(vec![
            declare(&world, find_first_child(global("workspace"), "World")),
            use_local(&world),
            declare(&seeds, guarded_find(RValue::Local(world.clone()), "PlantedSeeds")),
            use_local(&seeds),
            declare(&pots, guarded_find(RValue::Local(world.clone()), "PlacedPots")),
            use_local(&pots),
            declare(
                &seeds_client,
                guarded_find(RValue::Local(seeds.clone()), "Client"),
            ),
            use_local(&seeds_client),
            declare(
                &pots_client,
                guarded_find(RValue::Local(pots.clone()), "Client"),
            ),
            use_local(&pots_client),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&world), "world");
        assert_eq!(name_of(&seeds), "plantedSeeds");
        assert_eq!(name_of(&pots), "placedPots");
        assert_eq!(name_of(&seeds_client), "plantedSeedsClient");
        assert_eq!(name_of(&pots_client), "placedPotsClient");
    }

    /// The guarded-lookup hint (60) beats the GetDescendants->folder hint (55), so
    /// a lookup result that is later iterated still takes the lookup name.
    #[test]
    fn guarded_lookup_beats_get_descendants() {
        let world = RcLocal::default();
        let seeds = RcLocal::default();
        let index = RcLocal::default();
        let descendant = RcLocal::default();

        let loop_for = GenericFor::new(
            vec![index.clone(), descendant.clone()],
            vec![RValue::MethodCall(MethodCall::new(
                RValue::Local(seeds.clone()),
                "GetDescendants".to_string(),
                vec![],
            ))],
            Block(vec![use_local(&descendant)]),
        );

        let mut block = Block(vec![
            declare(&world, find_first_child(global("workspace"), "World")),
            use_local(&world),
            declare(&seeds, guarded_find(RValue::Local(world.clone()), "PlantedSeeds")),
            Statement::GenericFor(loop_for),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&seeds), "plantedSeeds");
    }

    /// A `... or default` tail is stripped: the local is named after the guarded
    /// lookup, never after the fallback.
    #[test]
    fn guarded_lookup_strips_or_default_tail() {
        let world = RcLocal::default();
        let visual = RcLocal::default();

        let guard = guarded_find(RValue::Local(world.clone()), "Visual");
        let with_default =
            RValue::Binary(Binary::new(guard, global("workspace"), BinaryOperation::Or));

        let mut block = Block(vec![
            declare(&world, find_first_child(global("workspace"), "World")),
            use_local(&world),
            declare(&visual, with_default),
            use_local(&visual),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&visual), "visual");
    }

    /// A dynamic (non-literal) lookup argument yields no hint — refusal-by-default
    /// keeps the generic name rather than inventing one.
    #[test]
    fn guarded_lookup_refuses_dynamic_arg() {
        let key = named_local("key");
        let result = RcLocal::default();

        let dynamic = RValue::Binary(Binary::new(
            global("folder"),
            RValue::MethodCall(MethodCall::new(
                global("folder"),
                "FindFirstChild".to_string(),
                vec![RValue::Local(key.clone())],
            )),
            BinaryOperation::And,
        ));

        let mut block = Block(vec![declare(&result, dynamic), use_local(&result)]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&result), "v");
    }

    /// A specific (non-generic) child stays bare even when the receiver is named —
    /// only the generic-child set is parent-qualified.
    #[test]
    fn guarded_lookup_specific_child_not_qualified() {
        let character = named_local("character");
        let part = RcLocal::default();

        let mut block = Block(vec![
            declare(
                &part,
                guarded_find(RValue::Local(character.clone()), "HumanoidRootPart"),
            ),
            use_local(&part),
            use_local(&character),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&part), "humanoidRootPart");
    }

    // `(primary) or fallback` — the `... or default` tail of a guarded lookup.
    fn or_fallback(primary: RValue) -> RValue {
        RValue::Binary(Binary::new(primary, boolean(false), BinaryOperation::Or))
    }

    /// A `... or default` tail must not defeat parent-qualification: the generic
    /// `Server` children still become `plantedSeedsServer`/`placedPotsServer`
    /// rather than colliding to `server`/`server2` (regression fixed after review).
    #[test]
    fn guarded_lookup_qualifies_through_or_default_tail() {
        let world = RcLocal::default();
        let seeds = RcLocal::default();
        let pots = RcLocal::default();
        let seeds_server = RcLocal::default();
        let pots_server = RcLocal::default();

        let mut block = Block(vec![
            declare(&world, find_first_child(global("workspace"), "World")),
            use_local(&world),
            declare(&seeds, guarded_find(RValue::Local(world.clone()), "PlantedSeeds")),
            use_local(&seeds),
            declare(&pots, guarded_find(RValue::Local(world.clone()), "PlacedPots")),
            use_local(&pots),
            declare(
                &seeds_server,
                or_fallback(guarded_find(RValue::Local(seeds.clone()), "Server")),
            ),
            use_local(&seeds_server),
            declare(
                &pots_server,
                or_fallback(guarded_find(RValue::Local(pots.clone()), "Server")),
            ),
            use_local(&pots_server),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&seeds_server), "plantedSeedsServer");
        assert_eq!(name_of(&pots_server), "placedPotsServer");
    }

    /// A bare `X:FindFirstChild("Name") or fallback` (no leading `and` guard) is
    /// still named after the primary lookup, not left as `v`.
    #[test]
    fn bare_lookup_or_fallback_is_named() {
        let remotes = named_local("remotes");
        let beanstalk = RcLocal::default();

        let lookup_or = RValue::Binary(Binary::new(
            find_first_child(RValue::Local(remotes.clone()), "Beanstalk"),
            RValue::MethodCall(MethodCall::new(
                RValue::Local(remotes.clone()),
                "WaitForChild".to_string(),
                vec![string("Beanstalk")],
            )),
            BinaryOperation::Or,
        ));

        let mut block = Block(vec![
            declare(&beanstalk, lookup_or),
            use_local(&beanstalk),
            use_local(&remotes),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&beanstalk), "beanstalk");
    }

    /// Qualification is skipped when the receiver name already ends with the child
    /// word, so `clientModel:FindFirstChildWhichIsA("Model")` reads as `model`,
    /// not the stuttering `clientModelModel`.
    #[test]
    fn guarded_lookup_avoids_stutter() {
        let client_model = named_local("clientModel");
        let model = RcLocal::default();

        let lookup = RValue::Binary(Binary::new(
            RValue::Local(client_model.clone()),
            RValue::MethodCall(MethodCall::new(
                RValue::Local(client_model.clone()),
                "FindFirstChildWhichIsA".to_string(),
                vec![string("Model")],
            )),
            BinaryOperation::And,
        ));

        let mut block = Block(vec![
            declare(&model, lookup),
            use_local(&model),
            use_local(&client_model),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&model), "model");
    }

    // ---- §2.1 param name inference from usage ----

    fn declare_closure_fn(param_fn: Function) -> (RcLocal, Statement) {
        let f = RcLocal::default();
        (f.clone(), declare(&f, closure_of(param_fn)))
    }

    /// A param `typeof`-guarded as a string reads as `value` (ground truth:
    /// ChatTipsClient `trimString(value)`), and a `~=` guard counts the same way.
    #[test]
    fn typeof_string_guard_names_param_value() {
        let p = RcLocal::default();
        let guard = RValue::Binary(Binary::new(
            RValue::Call(Call::new(global("typeof"), vec![RValue::Local(p.clone())])),
            string("string"),
            BinaryOperation::NotEqual,
        ));
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            If::new(guard, Block(vec![ret(vec![])]), Block::default()).into(),
            use_local(&p),
        ]);
        let (f, decl) = declare_closure_fn(function);
        let mut block = Block(vec![decl, use_local(&f)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "value");
    }

    /// A param checked against two different types is polymorphic -> stays `p`.
    #[test]
    fn typeof_conflict_keeps_default_param_name() {
        let p = RcLocal::default();
        let guard = |ty: &str| {
            RValue::Binary(Binary::new(
                RValue::Call(Call::new(global("typeof"), vec![RValue::Local(p.clone())])),
                string(ty),
                BinaryOperation::Equal,
            ))
        };
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            If::new(guard("string"), Block(vec![use_local(&p)]), Block::default()).into(),
            If::new(guard("number"), Block(vec![use_local(&p)]), Block::default()).into(),
        ]);
        let (f, decl) = declare_closure_fn(function);
        let mut block = Block(vec![decl, use_local(&f)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "p");
    }

    /// A param used as the receiver of an instance method reads as `instance`.
    #[test]
    fn instance_method_receiver_names_param_instance() {
        let p = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![Statement::Call(Call::new(
            global("print"),
            vec![RValue::MethodCall(MethodCall::new(
                RValue::Local(p.clone()),
                "GetChildren".to_string(),
                vec![],
            ))],
        ))]);
        let (f, decl) = declare_closure_fn(function);
        let mut block = Block(vec![decl, use_local(&f)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "instance");
    }

    /// `:IsA("Class")` (score 55) beats the generic instance-shape hint (42).
    #[test]
    fn isa_class_beats_instance_shape() {
        let p = RcLocal::default();
        let isa = RValue::MethodCall(MethodCall::new(
            RValue::Local(p.clone()),
            "IsA".to_string(),
            vec![string("BasePart")],
        ));
        let get = RValue::MethodCall(MethodCall::new(
            RValue::Local(p.clone()),
            "GetChildren".to_string(),
            vec![],
        ));
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            If::new(isa, Block(vec![use_local(&p)]), Block::default()).into(),
            Statement::Call(Call::new(global("print"), vec![get])),
        ]);
        let (f, decl) = declare_closure_fn(function);
        let mut block = Block(vec![decl, use_local(&f)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "part");
    }

    /// A param read via `.UserId` reads as `player`.
    #[test]
    fn player_field_names_param_player() {
        let p = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![Statement::Call(Call::new(
            global("print"),
            vec![field(&p, "UserId")],
        ))]);
        let (f, decl) = declare_closure_fn(function);
        let mut block = Block(vec![decl, use_local(&f)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "player");
    }

    /// A bare `RunService.Heartbeat:Connect(function(p) ...)` names `p` -> `dt`.
    #[test]
    fn heartbeat_callback_param_named_dt() {
        let p = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![use_local(&p)]);
        let connect = Statement::MethodCall(MethodCall::new(
            RValue::Index(Index::new(global("RunService"), string("Heartbeat"))),
            "Connect".to_string(),
            vec![closure_of(function)],
        ));
        let mut block = Block(vec![connect]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "dt");
    }

    /// An assigned `InputBegan:Connect(function(p, p2) ...)` names the params from
    /// the signature: `input`, `gameProcessed`.
    #[test]
    fn input_began_callback_params_named() {
        let p = RcLocal::default();
        let p2 = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![p.clone(), p2.clone()];
        function.body = Block(vec![use_local(&p), use_local(&p2)]);
        let conn = RcLocal::default();
        let connect = RValue::MethodCall(MethodCall::new(
            RValue::Index(Index::new(global("UserInputService"), string("InputBegan"))),
            "Connect".to_string(),
            vec![closure_of(function)],
        ));
        let mut block = Block(vec![declare(&conn, connect), use_local(&conn)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "input");
        assert_eq!(name_of(&p2), "gameProcessed");
    }

    /// A `table.sort` comparator's two params read as `a`/`b`.
    #[test]
    fn table_sort_comparator_params_named_a_b() {
        let a = RcLocal::default();
        let b = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![a.clone(), b.clone()];
        function.body = Block(vec![ret(vec![RValue::Binary(Binary::new(
            RValue::Local(a.clone()),
            RValue::Local(b.clone()),
            BinaryOperation::LessThan,
        ))])]);
        let sort = Statement::Call(Call::new(
            RValue::Index(Index::new(global("table"), string("sort"))),
            vec![global("items"), closure_of(function)],
        ));
        let mut block = Block(vec![sort]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&a), "a");
        assert_eq!(name_of(&b), "b");
    }

    /// A non-event method receiver (`Changed` is overloaded -> not in the dict)
    /// does NOT get a fabricated callback-param name.
    #[test]
    fn unknown_event_does_not_name_callback_param() {
        let p = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![use_local(&p)]);
        let connect = Statement::MethodCall(MethodCall::new(
            RValue::Index(Index::new(global("part"), string("Changed"))),
            "Connect".to_string(),
            vec![closure_of(function)],
        ));
        let mut block = Block(vec![connect]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "p");
    }

    // A `typeof(p)`-guarded param. Helper builds `local function f(p) if
    // typeof(p) == ty then use(p) end use(p) end`.
    fn typeof_guarded_param(ty: &str) -> (RcLocal, Block) {
        let p = RcLocal::default();
        let guard = RValue::Binary(Binary::new(
            RValue::Call(Call::new(global("typeof"), vec![RValue::Local(p.clone())])),
            string(ty),
            BinaryOperation::Equal,
        ));
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            If::new(guard, Block(vec![use_local(&p)]), Block::default()).into(),
            use_local(&p),
        ]);
        let (f, decl) = declare_closure_fn(function);
        (p, Block(vec![decl, use_local(&f)]))
    }

    /// `typeof(p) == "Instance"` reads as `instance`.
    #[test]
    fn typeof_instance_guard_names_param_instance() {
        let (p, mut block) = typeof_guarded_param("Instance");
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "instance");
    }

    /// `typeof(p) == "function"` reads as `callback`.
    #[test]
    fn typeof_function_guard_names_param_callback() {
        let (p, mut block) = typeof_guarded_param("function");
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "callback");
    }

    /// `RunService.Stepped:Connect(function(p, p2))` -> `time`, `dt` (two slots).
    #[test]
    fn stepped_callback_params_named_time_dt() {
        let p = RcLocal::default();
        let p2 = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![p.clone(), p2.clone()];
        function.body = Block(vec![use_local(&p), use_local(&p2)]);
        let connect = Statement::MethodCall(MethodCall::new(
            RValue::Index(Index::new(global("RunService"), string("Stepped"))),
            "Connect".to_string(),
            vec![closure_of(function)],
        ));
        let mut block = Block(vec![connect]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "time");
        assert_eq!(name_of(&p2), "dt");
    }

    /// `AncestryChanged:Connect(function(p, p2))` keeps slot 0 default (signature
    /// `None`) and names slot 1 `parent`.
    #[test]
    fn ancestry_changed_names_second_param_parent_only() {
        let p = RcLocal::default();
        let p2 = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![p.clone(), p2.clone()];
        function.body = Block(vec![use_local(&p), use_local(&p2)]);
        let connect = Statement::MethodCall(MethodCall::new(
            RValue::Index(Index::new(global("part"), string("AncestryChanged"))),
            "Connect".to_string(),
            vec![closure_of(function)],
        ));
        let mut block = Block(vec![connect]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "p");
        assert_eq!(name_of(&p2), "parent");
    }

    /// `:Once` and `:ConnectParallel` are recognised exactly like `:Connect`.
    #[test]
    fn heartbeat_once_and_connect_parallel_name_dt() {
        for method in ["Once", "ConnectParallel"] {
            let p = RcLocal::default();
            let mut function = Function::default();
            function.parameters = vec![p.clone()];
            function.body = Block(vec![use_local(&p)]);
            let connect = Statement::MethodCall(MethodCall::new(
                RValue::Index(Index::new(global("RunService"), string("Heartbeat"))),
                method.to_string(),
                vec![closure_of(function)],
            ));
            let mut block = Block(vec![connect]);
            name_locals(&mut block, true);
            assert_eq!(name_of(&p), "dt", "method {method}");
        }
    }

    /// `props` (50) wins over the instance-shape hint (42) for a component param
    /// that is both read as a record and used as an instance receiver.
    #[test]
    fn props_beats_instance_shape() {
        let p = RcLocal::default();
        let (a, b, c) = (RcLocal::default(), RcLocal::default(), RcLocal::default());
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            declare(&a, field(&p, "visible")),
            declare(&b, field(&p, "currentTabId")),
            declare(&c, field(&p, "onClose")),
            Statement::Call(Call::new(
                global("print"),
                vec![RValue::MethodCall(MethodCall::new(
                    RValue::Local(p.clone()),
                    "FindFirstChild".to_string(),
                    vec![string("X")],
                ))],
            )),
            ret(vec![create_element(vec![
                string("Frame"),
                RValue::Table(Table::default()),
            ])]),
        ]);
        let comp = RcLocal::default();
        let mut block = Block(vec![declare(&comp, closure_of(function)), use_local(&comp)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "props");
    }

    /// A param used as an instance receiver but ALSO `typeof`-guarded as a scalar
    /// is a contradiction -> emit nothing -> stays `p`.
    #[test]
    fn instance_shape_with_typeof_scalar_stays_default() {
        let p = RcLocal::default();
        let guard = RValue::Binary(Binary::new(
            RValue::Call(Call::new(global("typeof"), vec![RValue::Local(p.clone())])),
            string("string"),
            BinaryOperation::Equal,
        ));
        let get_children = RValue::MethodCall(MethodCall::new(
            RValue::Local(p.clone()),
            "GetChildren".to_string(),
            vec![],
        ));
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            If::new(guard, Block(vec![use_local(&p)]), Block::default()).into(),
            Statement::Call(Call::new(global("print"), vec![get_children])),
        ]);
        let (f, decl) = declare_closure_fn(function);
        let mut block = Block(vec![decl, use_local(&f)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "p");
    }

    fn predicate_call(callee: &str) -> RValue {
        RValue::Call(Call::new(global(callee), vec![global("x")]))
    }

    fn bool_compare(value: RValue, literal: RValue, op: BinaryOperation) -> RValue {
        RValue::Binary(Binary::new(value, literal, op))
    }

    fn index_of(base: &str, key: &str) -> RValue {
        RValue::Index(Index::new(global(base), string(key)))
    }

    #[test]
    fn predicate_is_prefix_names_subject() {
        let v = RcLocal::default();
        let mut block = Block(vec![
            declare(&v, predicate_call("isGraphicsDisabled")),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "graphicsDisabled");
    }

    #[test]
    fn predicate_has_prefix_names_subject() {
        let v = RcLocal::default();
        let mut block = Block(vec![declare(&v, predicate_call("hasOwner")), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "owner");
    }

    /// `is`/`has` with nothing after the verb is not a predicate -> default name.
    #[test]
    fn predicate_bare_verb_refused() {
        let v = RcLocal::default();
        let mut block = Block(vec![declare(&v, predicate_call("is")), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// `island` is `is` + lowercase -> not a predicate -> default name.
    #[test]
    fn predicate_lowercase_after_prefix_refused() {
        let v = RcLocal::default();
        let mut block = Block(vec![declare(&v, predicate_call("island")), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// `isEnd` strips to `End` -> `end`, a Lua keyword -> sanitize refuses -> default.
    #[test]
    fn predicate_keyword_stem_refused() {
        let v = RcLocal::default();
        let mut block = Block(vec![declare(&v, predicate_call("isEnd")), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// A non-predicate call (`getThing`) is left alone by Layer A.
    #[test]
    fn predicate_non_predicate_call_refused() {
        let v = RcLocal::default();
        let mut block = Block(vec![declare(&v, predicate_call("getThing")), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// The callee is a recovered `local function isReady` reference; its name lives
    /// on the closure hint set earlier in the collect, so Layer A resolves it.
    #[test]
    fn predicate_local_function_callee_resolves() {
        let is_ready = RcLocal::default();
        let mut function = Function::default();
        function.name = Some("isReady".to_string());
        let v = RcLocal::default();
        let mut block = Block(vec![
            declare(&is_ready, closure_of(function)),
            declare(
                &v,
                RValue::Call(Call::new(RValue::Local(is_ready.clone()), vec![global("x")])),
            ),
            use_local(&v),
            use_local(&is_ready),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "ready");
    }

    /// `local v1, v2 = canFn(...)` — Layer A only names the first lvalue (the extra
    /// return slot has no paired rvalue). Uses an `is` predicate to keep it firing.
    #[test]
    fn predicate_tuple_names_only_first() {
        let v1 = RcLocal::default();
        let v2 = RcLocal::default();
        let mut assign = Assign::new(
            vec![LValue::Local(v1.clone()), LValue::Local(v2.clone())],
            vec![predicate_call("isReady")],
        );
        assign.prefix = true;
        let mut block = Block(vec![assign.into(), use_local(&v1), use_local(&v2)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v1), "ready");
        assert_eq!(name_of(&v2), "v");
    }

    #[test]
    fn bool_field_eq_true_names_field() {
        let v = RcLocal::default();
        let cmp = bool_compare(index_of("obj", "Visible"), boolean(true), BinaryOperation::Equal);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "visible");
    }

    /// `X.Locked == false` is negated polarity (the value is true when Locked is
    /// FALSE), so naming it `locked` would mislead -> refused.
    #[test]
    fn bool_field_eq_false_refused() {
        let v = RcLocal::default();
        let cmp = bool_compare(index_of("obj", "Locked"), boolean(false), BinaryOperation::Equal);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// `X.Enabled ~= false` is positive polarity (true when Enabled is truthy — the
    /// default-true idiom), matching source (`UseFXTop ~= false` -> `useFXTop`).
    #[test]
    fn bool_field_neq_false_names_field() {
        let v = RcLocal::default();
        let cmp = bool_compare(index_of("obj", "Enabled"), boolean(false), BinaryOperation::NotEqual);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "enabled");
    }

    /// `X.Favorite ~= true` is negated polarity (the value is the INVERSE of the
    /// field — it is the toggled/next state), so naming it `favorite` would mislead
    /// (source calls such a result `newState`) -> refused.
    #[test]
    fn bool_field_neq_true_refused() {
        let v = RcLocal::default();
        let cmp = bool_compare(index_of("obj", "Favorite"), boolean(true), BinaryOperation::NotEqual);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// `X.Field ~= nil` is a boolean that is NOT the field — must NOT be named after
    /// it (source calls `Parent ~= nil` `hadParent`). `nil` is not a boolean literal,
    /// so it is excluded by construction.
    #[test]
    fn bool_field_neq_nil_refused() {
        let v = RcLocal::default();
        let cmp = bool_compare(
            index_of("obj", "Parent"),
            RValue::Literal(Literal::Nil),
            BinaryOperation::NotEqual,
        );
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    #[test]
    fn bool_field_eq_nil_refused() {
        let v = RcLocal::default();
        let cmp = bool_compare(
            index_of("obj", "Color"),
            RValue::Literal(Literal::Nil),
            BinaryOperation::Equal,
        );
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// `X.Count == 5` compares against a number, not a boolean -> not named.
    #[test]
    fn bool_field_eq_number_refused() {
        let v = RcLocal::default();
        let cmp = bool_compare(index_of("obj", "Count"), number(5.0), BinaryOperation::Equal);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// A bare global (`count == true`) has no field key -> not named.
    #[test]
    fn bool_non_index_lhs_refused() {
        let v = RcLocal::default();
        let cmp = bool_compare(global("count"), boolean(true), BinaryOperation::Equal);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// A leading `_` (private marker) is dropped: `obj._isOpen == true` -> `isOpen`.
    #[test]
    fn bool_field_leading_underscore_stripped() {
        let v = RcLocal::default();
        let cmp = bool_compare(index_of("obj", "_isOpen"), boolean(true), BinaryOperation::Equal);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "isOpen");
    }

    /// Defensive: a boolean literal on the LEFT (`true == X.Visible`) still names
    /// after the field. (Corpus always has the literal on the right, but the code
    /// handles both orders.)
    #[test]
    fn bool_field_literal_on_left_names_field() {
        let v = RcLocal::default();
        let cmp = bool_compare(boolean(true), index_of("obj", "Visible"), BinaryOperation::Equal);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "visible");
    }

    /// `inst:GetAttribute("IsPlanted") == true` -> `isPlanted` (named after the
    /// attribute string; NOT stem-stripped, matching source).
    #[test]
    fn bool_attribute_eq_true_names_attribute() {
        let v = RcLocal::default();
        let getattr = RValue::MethodCall(MethodCall::new(
            global("inst"),
            "GetAttribute".to_string(),
            vec![string("IsPlanted")],
        ));
        let cmp = bool_compare(getattr, boolean(true), BinaryOperation::Equal);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "isPlanted");
    }

    #[test]
    fn bool_attribute_leading_underscore_stripped() {
        let v = RcLocal::default();
        let getattr = RValue::MethodCall(MethodCall::new(
            global("inst"),
            "GetAttribute".to_string(),
            vec![string("__StaticMode")],
        ));
        let cmp = bool_compare(getattr, boolean(true), BinaryOperation::Equal);
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "staticMode");
    }

    /// A bare reassignment `v = X.Field == true` (NOT a `local` declaration) must
    /// keep its default name — it is typically the arm of a `conditional_expressions`
    /// diamond (`local v if c then v = A else v = false end; return v`) that the later
    /// pass collapses to `c and A`; naming it would suppress that collapse.
    #[test]
    fn bool_compare_reassignment_not_named() {
        let v = RcLocal::default();
        let reassign = Assign::new(
            vec![LValue::Local(v.clone())],
            vec![bool_compare(
                index_of("obj", "Visible"),
                boolean(true),
                BinaryOperation::Equal,
            )],
        ); // prefix defaults to false -> a reassignment, not a declaration
        let mut block = Block(vec![
            declare(&v, RValue::Literal(Literal::Nil)),
            reassign.into(),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// Layer A predicate naming likewise only fires on declarations, not a bare
    /// reassignment `v = isFoo(x)`.
    #[test]
    fn predicate_reassignment_not_named() {
        let v = RcLocal::default();
        let reassign = Assign::new(
            vec![LValue::Local(v.clone())],
            vec![predicate_call("isReady")],
        );
        let mut block = Block(vec![
            declare(&v, RValue::Literal(Literal::Nil)),
            reassign.into(),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// Layer B (Equal/NotEqual) must not disturb the And/Or guarded-lookup path:
    /// `x and x:FindFirstChild("Humanoid")` still names after the lookup.
    #[test]
    fn bool_compare_does_not_disturb_guarded_lookup() {
        let v = RcLocal::default();
        let lookup = RValue::Binary(Binary::new(
            global("x"),
            RValue::MethodCall(MethodCall::new(
                global("x"),
                "FindFirstChild".to_string(),
                vec![string("Humanoid")],
            )),
            BinaryOperation::And,
        ));
        let mut block = Block(vec![declare(&v, lookup), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "humanoid");
    }

    // ----- §2.1-locals: RHS-driven local naming rules -----

    fn reassign(local: &RcLocal, value: RValue) -> Statement {
        let mut assign = Assign::new(vec![LValue::Local(local.clone())], vec![value]);
        assign.prefix = false;
        assign.into()
    }

    fn method_call(receiver: RValue, method: &str, args: Vec<RValue>) -> RValue {
        RValue::MethodCall(MethodCall::new(receiver, method.to_string(), args))
    }

    fn call(callee: RValue, args: Vec<RValue>) -> RValue {
        RValue::Call(Call::new(callee, args))
    }

    #[test]
    fn names_clone_connection_track() {
        let c = RcLocal::default();
        let conn = RcLocal::default();
        let track = RcLocal::default();
        let mut block = Block(vec![
            declare(&c, method_call(global("inst"), "Clone", vec![])),
            use_local(&c),
            declare(&conn, method_call(global("sig"), "Connect", vec![])),
            use_local(&conn),
            declare(&track, method_call(global("humanoid"), "LoadAnimation", vec![])),
            use_local(&track),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&c), "clone");
        assert_eq!(name_of(&conn), "connection");
        assert_eq!(name_of(&track), "track");
    }

    #[test]
    fn names_get_attribute_after_its_key_not_attribute() {
        let v = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &v,
                method_call(global("inst"), "GetAttribute", vec![string("OwnerId")]),
            ),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        // Must be the attribute key, not the generic "attribute" the Get-prefix
        // getter rule would otherwise produce.
        assert_eq!(name_of(&v), "ownerId");
    }

    #[test]
    fn names_tonumber_after_inner_field() {
        let v = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &v,
                call(global("tonumber"), vec![RValue::Index(Index::new(
                    global("config"),
                    string("PlaceId"),
                ))]),
            ),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "placeId");
    }

    #[test]
    fn tonumber_of_bare_local_or_literal_stays_default() {
        // No name signal in the argument -> no hint -> default `v`.
        let v = RcLocal::default();
        let mut block = Block(vec![
            declare(&v, call(global("tonumber"), vec![number(5.0)])),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    #[test]
    fn names_bare_time_as_timestamp() {
        let a = RcLocal::default();
        let b = RcLocal::default();
        let mut block = Block(vec![
            declare(&a, call(RValue::Index(Index::new(global("os"), string("clock"))), vec![])),
            use_local(&a),
            declare(&b, call(global("tick"), vec![])),
            use_local(&b),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&a), "timestamp");
        assert_eq!(name_of(&b), "timestamp2");
    }

    #[test]
    fn names_color3_constructor() {
        let v = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &v,
                call(
                    RValue::Index(Index::new(global("Color3"), string("fromRGB"))),
                    vec![number(255.0), number(0.0), number(0.0)],
                ),
            ),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        // The trailing digit of `Color3` is dropped so disambiguating suffixes
        // read cleanly (`color`, `color2`, ... not `color3`, `color32`).
        assert_eq!(name_of(&v), "color");
    }

    #[test]
    fn index_field_named_self_is_rejected() {
        // `local v = t.self` must NOT yield a local named `self` (would break
        // §2.8 colon-method recovery); it falls back to the default `v`.
        let v = RcLocal::default();
        let mut block = Block(vec![
            declare(&v, RValue::Index(Index::new(global("t"), string("self")))),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    #[test]
    fn conditional_diamond_arm_is_not_named() {
        // `local v; if c then v = Color3.fromRGB(..) else v = Color3.fromRGB(..) end; use(v)`
        // is a conditional_expressions collapse candidate (reads==1, writes==3).
        // Naming the arm RHS would set `is_generated_temp(v)` false and suppress
        // the later collapse, so it must stay the generated `v`.
        let v = RcLocal::default();
        let arm = |r, g, b| {
            reassign(
                &v,
                call(
                    RValue::Index(Index::new(global("Color3"), string("fromRGB"))),
                    vec![number(r), number(g), number(b)],
                ),
            )
        };
        let mut block = Block(vec![
            declare(&v, RValue::Literal(Literal::Nil)),
            Statement::If(crate::If::new(
                global("cond"),
                Block(vec![arm(255.0, 0.0, 0.0)]),
                Block(vec![arm(0.0, 255.0, 0.0)]),
            )),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    #[test]
    fn non_adjacent_diamond_arm_is_still_named() {
        // Same counts (reads==1, writes==3) as a real diamond, but a statement
        // sits between the decl and the `if`, so conditional_expressions never
        // collapses it (it requires the `if` at decl+1). The STRUCTURAL gate (not
        // just the count gate) must therefore allow naming: v -> "clone".
        let v = RcLocal::default();
        let arm = || reassign(&v, method_call(global("inst"), "Clone", vec![]));
        let mut block = Block(vec![
            declare(&v, RValue::Literal(Literal::Nil)),
            Statement::Call(Call::new(global("print"), vec![string("sep")])),
            Statement::If(crate::If::new(
                global("cond"),
                Block(vec![arm()]),
                Block(vec![arm()]),
            )),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "clone");
    }

    #[test]
    fn tostring_recurses_and_two_arg_tonumber_refused() {
        let a = RcLocal::default();
        let b = RcLocal::default();
        let mut block = Block(vec![
            // tostring(inst:GetAttribute("OwnerId")) -> "ownerId"
            declare(
                &a,
                call(
                    global("tostring"),
                    vec![method_call(global("inst"), "GetAttribute", vec![string("OwnerId")])],
                ),
            ),
            use_local(&a),
            // tonumber(x, 16): 2 args -> no name signal -> stays default.
            declare(&b, call(global("tonumber"), vec![global("x"), number(16.0)])),
            use_local(&b),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&a), "ownerId");
        assert_eq!(name_of(&b), "v");
    }

    #[test]
    fn color3_alternate_constructor_named_color() {
        let v = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &v,
                call(
                    RValue::Index(Index::new(global("Color3"), string("fromHSV"))),
                    vec![number(0.5), number(1.0), number(1.0)],
                ),
            ),
            use_local(&v),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "color");
    }
}
