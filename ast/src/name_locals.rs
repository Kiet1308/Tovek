use itertools::Either;
use rustc_hash::{FxHashMap, FxHashSet};
use triomphe::Arc;

use crate::{
    inline_temps::{collect_usage, Usage},
    Binary, BinaryOperation, Block, Call, Index, LValue, Literal, Local, LocalRw, MethodCall,
    RValue, RcLocal, Select, Statement, Table, Traverse,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NameLocalOptions {
    pub dont_reuse_var: bool,
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
    // SCREAMING_SNAKE_CASE is already a deliberate, readable source naming
    // convention. Lowercasing only its first character would manufacture the
    // malformed hybrid `dEFAULT_BRUSH`. This check is intentionally performed
    // on the sanitized identifier so every character considered here is one we
    // can actually emit.
    let is_constant = chars.iter().any(|c| c.is_ascii_uppercase())
        && chars
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_');
    match case {
        IdentifierCase::LowerCamel if chars[0].is_ascii_uppercase() && !is_constant => {
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

/// The param name derived from a field-store destination KEY (`obj.Key = param`)
/// or an attribute key (`obj:SetAttribute("Key", param)`) — both are a literal
/// source identifier naming the value written through them, so they share this
/// helper. Strips a leading `_` (private-field convention: `_balance` ->
/// `balance`) and a trailing type/index digit (`BackgroundColor3` ->
/// `backgroundColor`), and refuses keys no more informative than the honest
/// default `p`: a generic-content word, a single letter, or junk that fails
/// `sanitize`. NO singularization — `self.items = p` means `p` *is* the
/// collection, so `items` is the right name.
fn param_name_from_field_key(key: &str) -> Option<String> {
    // Keys that carry no more meaning than `p`. `name`/`text`/`parent`/etc. are
    // deliberately NOT here — a param written to `.Name`/`.Text` is genuinely a
    // name/text, which is more informative than `p`.
    const GENERIC_FIELD_KEYS: &[&str] = &[
        "value", "val", "v", "data", "item", "key", "index", "self", "type", "result", "arg", "n",
    ];
    let sanitized = sanitize(key.trim_start_matches('_'))?;
    // Strip a trailing type/index digit exactly as `constructor_type_name` does
    // (`BackgroundColor3` -> `backgroundColor`, `Part0` -> `part`): kept, the
    // digit chains into a misleading doubly-numeric `backgroundColor33` once the
    // collision disambiguator appends its own counter. Only strip when a
    // multi-char stem survives, so an all-digit key keeps its sanitized form.
    let trimmed = sanitized.trim_end_matches(|c: char| c.is_ascii_digit());
    let name = if trimmed.len() >= 2 {
        trimmed.to_string()
    } else {
        sanitized
    };
    if name.len() < 2 || GENERIC_FIELD_KEYS.contains(&name.as_str()) {
        return None;
    }
    Some(name)
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
    // A bare time read is an instantaneous sample. The whole-tree usage pass
    // upgrades it to `lastTime` when it is later used as the subtraction base in
    // `os.clock() - local`; keeping the RHS-only fallback as `now` avoids calling
    // every unrelated clock sample a persistent timestamp.
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
            return Some("now".to_string());
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
        if matches!(
            method,
            b"fromRGB" | b"fromHSV" | b"fromHex" | b"fromName" | b"fromAxis"
        ) {
            return constructor_type_name(&index.left);
        }
    }
    None
}

fn instance_constructor_hint(rvalue: &RValue) -> Option<String> {
    let (RValue::Call(call) | RValue::Select(Select::Call(call))) = rvalue else {
        return None;
    };
    let RValue::Index(callee) = &*call.value else {
        return None;
    };
    if global_name(&callee.left) != Some("Instance") || index_key(callee) != Some("new") {
        return None;
    }
    call.arguments
        .first()
        .and_then(string_literal)
        .and_then(sanitize)
}

fn is_instance_compatible_placeholder(rvalue: &RValue) -> bool {
    matches!(rvalue, RValue::Literal(Literal::Nil))
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

/// The descriptive subject of a factory/getter function name: `getOwnPlot` ->
/// `OwnPlot`, `createButton` -> `Button`, `normalizeContentId` -> `ContentId`.
/// Returns `None` unless the name starts with an allow-listed verb *immediately
/// followed by an uppercase letter*, so non-verbs (`getter`, `island`) and bare
/// verbs (`get`) are left alone. The remainder keeps its original case for the
/// caller to `sanitize` into lowerCamel.
///
/// A compound factory verb (`getOrCreate`/`findOrCreate`/...) is stripped as a
/// *whole phrase* — `getOrCreateFXPart` -> `fXPart`, NOT `orCreateFXPart` (a
/// garbage identifier). The allow-list is deliberately small and ground-truth
/// verified: only verbs that strip to a noun reading as the produced value are
/// included. Notably `summarize` is excluded (`summarizeStatus` -> `status`
/// inverts a summary-of-status into a status), as are weak/ambiguous verbs
/// (`process`, `apply`, `use`, `handle`, `update`).
///
/// A stripped remainder whose leading PascalCase word is a preposition or
/// conjunction is REFUSED (`cloneFromNode` -> `FromNode` -> *refused*, keeps `v`;
/// `cloneAndPosition` -> `AndPosition` -> *refused*): such a name reads as a
/// qualifier ("from node", "and position"), not as the produced value, so it is
/// worse than the generic `vN`. Catches the verb+preposition+noun shape the
/// compound `getOr…`/`findOr…` rule does not.
fn strip_verb_prefix(name: &str) -> Option<&str> {
    // Accept a stripped remainder only if it is a noun-like PascalCase word — it
    // must be uppercase-led AND not begin with a connective word.
    fn noun_like(rest: &str) -> Option<&str> {
        if rest.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            && !starts_with_connective(rest)
        {
            Some(rest)
        } else {
            None
        }
    }
    // Compound factory verbs first: strip the leading `getOr`/`findOr` AND the
    // following factory verb (`Create`/`Make`/...) so only the noun survives.
    // `getOrCreate` (no trailing noun) -> refused (the noun-tail check fails).
    const COMPOUND: &[&str] = &["getOr", "findOr", "getAnd", "findAnd"];
    const SECOND_VERBS: &[&str] = &["Create", "Make", "Build", "Get", "Find", "Spawn"];
    for compound in COMPOUND {
        if let Some(rest) = name.strip_prefix(compound)
            && rest.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        {
            for verb in SECOND_VERBS {
                if let Some(tail) = rest.strip_prefix(verb) {
                    return noun_like(tail);
                }
            }
            return noun_like(rest);
        }
    }
    const VERBS: &[&str] = &[
        "get",
        "find",
        "create",
        "clone",
        "resolve",
        "ensure",
        "build",
        "make",
        "normalize",
    ];
    for verb in VERBS {
        if let Some(rest) = name.strip_prefix(verb)
            && rest.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        {
            return noun_like(rest);
        }
    }
    None
}

/// Whether a PascalCase remainder begins with a preposition/conjunction word
/// (`FromNode` -> yes via `From`; `AndPosition` -> yes via `And`). Only the
/// *whole* leading word counts, so genuine nouns that merely start with those
/// letters are unaffected (`Information` != `In`, `Output` != `Out`,
/// `Inventory` != `In`). Used to refuse verb-strips that would read as a
/// qualifier instead of the produced value.
fn starts_with_connective(rest: &str) -> bool {
    const CONNECTIVES: &[&str] = &[
        "and", "or", "from", "to", "with", "by", "of", "in", "for", "on", "into", "out", "off",
        "via", "at", "as", "the", "a",
    ];
    let mut chars = rest.chars();
    let mut word = String::new();
    if let Some(first) = chars.next() {
        word.push(first.to_ascii_lowercase());
    }
    for c in chars {
        if c.is_ascii_uppercase() {
            break;
        }
        word.push(c);
    }
    CONNECTIVES.contains(&word.as_str())
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
        // A `:Raycast(...)` result is a `RaycastResult` regardless of the receiver
        // (Workspace/WorldRoot), so the type-accurate name is unconditional. Source
        // commonly names it `result`; `raycastResult` is the unambiguous, never-
        // misleading form (it *is* a RaycastResult) and avoids colliding with the
        // generic `result` minted by the pcall-tuple / loop-fill hints.
        "Raycast" => return Some("raycastResult".to_string()),
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

/// A dynamic child-name lookup cannot recover the concrete source name, but its
/// assigned result is still unambiguously a child Instance. Kept outside
/// [`rvalue_hint`] for two reasons: path expressions passed to `require` yield a
/// module export, not the child itself; and the generic `child` hypernym must use
/// a lower score than concrete body evidence such as `result:IsA("BasePart")`.
fn dynamic_child_lookup_rvalue_hint(rvalue: &RValue) -> Option<String> {
    fn lookup(rvalue: &RValue) -> Option<&MethodCall> {
        match rvalue {
            RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
                Some(method_call)
            }
            // `receiver and receiver:FindFirstChild(name)` only nil-guards the
            // receiver; the yielded truthy value is still the lookup result.
            RValue::Binary(binary) if binary.operation == BinaryOperation::And => {
                lookup(&binary.right)
            }
            // An `or nil` tail preserves the child-or-nil domain. Any other
            // fallback may change the value's type and must not inherit `child`.
            RValue::Binary(binary)
                if binary.operation == BinaryOperation::Or
                    && matches!(&*binary.right, RValue::Literal(Literal::Nil)) =>
            {
                lookup(&binary.left)
            }
            _ => None,
        }
    }

    let method_call = lookup(rvalue)?;
    (CHILD_LOOKUP_METHODS.contains(&method_call.method.as_str())
        && !method_call.arguments.is_empty()
        && method_call.arguments.first().and_then(string_literal).is_none())
    .then(|| "child".to_string())
}

/// Name a local after the operand its short-circuit expression *yields*: `A or B`
/// evaluates to its LEFT (primary) operand when truthy, `A and B` to its RIGHT
/// (guarded) operand. The chosen operand is named through the general
/// [`rvalue_hint`], so a field-access primary names uniformly
/// (`localPlayer.Character or localPlayer.CharacterAdded:Wait()` -> `character`),
/// a global names after the global, and a nested short-circuit chain recurses —
/// while the nil-guard `inst and inst:FindFirstChild("X")` keeps the method-call
/// name it always had (`rvalue_hint` routes the `MethodCall` operand through
/// `method_call_hint`, a strict superset of the old method-call-only behaviour).
///
/// `binary_lookup_method_call` above is intentionally kept: it is still used by
/// `guarded_lookup_qualified_hint`, which needs the `&MethodCall` itself to
/// parent-qualify a generic child lookup. Recursion terminates because each step
/// descends into a strictly smaller `Box<RValue>` subtree of a finite AST.
fn binary_value_hint(binary: &Binary) -> Option<String> {
    match binary.operation {
        BinaryOperation::Or => rvalue_hint(&binary.left),
        BinaryOperation::And => rvalue_hint(&binary.right),
        _ => None,
    }
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

fn is_generic_semantic_name(name: &str) -> bool {
    is_default_name(name)
        || matches!(
            name,
            "_" | "i" | "j" | "k" | "n" | "x" | "y" | "fn" | "key" | "value" | "item" | "result"
        )
}

fn is_constant_identifier(name: &str) -> bool {
    let mut has_letter = false;
    !name.is_empty()
        && name.chars().all(|character| {
            has_letter |= character.is_ascii_uppercase();
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
        && has_letter
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
        // A short-circuit value expression is named after the operand it yields:
        // `A or B` -> A (primary), `A and B` -> B (guarded). So the nil-guard
        // `folder and folder:FindFirstChild("Client")` is named after its lookup,
        // and `localPlayer.Character or localPlayer.CharacterAdded:Wait()` after
        // its primary field (`character`).
        RValue::Binary(binary) => binary_value_hint(binary),
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

pub(crate) fn script_module_hint(script_name: &str) -> Option<String> {
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

    let compound = stem
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            first.to_ascii_uppercase().to_string() + chars.as_str()
        })
        .collect::<String>();
    sanitize_pascal(&compound)
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
        "status",
        "data",
        "address",
        "class",
        "process",
        "bonus",
        "physics",
        "analysis",
        "axis",
        "props",
        "series",
        "species",
        "news",
        "progress",
        "pass",
        "mass",
        "boss",
        "loss",
        "glass",
        "lens",
        "gas",
        "basis",
        "access",
        "success",
        "focus",
        "bias",
        "canvas",
        "radius",
        "virus",
        "index",
        "this",
        "kudos",
        // Latin/irregular plurals our drop-`s`/`-es` rules would mangle
        "indices",
        "vertices",
        "matrices",
        "analyses",
        "axes",
        "crises",
        "bases",
        "theses",
        "diagnoses",
        "hypotheses",
        "parentheses",
        // singular ends in `ie`, so `ies`->`y` is wrong (movie -> "movy")
        "movies",
        "cookies",
        "zombies",
        "rookies",
        "newbies",
        "selfies",
        "calories",
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

fn pluralize(name: &str) -> Option<String> {
    if !name.is_ascii() || name.is_empty() {
        return None;
    }
    if matches!(name, "children" | "people" | "data") {
        return Some(name.to_string());
    }
    if singularize(name).is_some() {
        return Some(name.to_string());
    }
    if name == "visible" {
        return Some("visibility".to_string());
    }
    if let Some(stem) = name.strip_suffix("Entry") {
        return Some(format!("{stem}Entries"));
    }
    if name == "child" {
        return Some("children".to_string());
    }
    if name == "person" {
        return Some("people".to_string());
    }
    let bytes = name.as_bytes();
    if name.ends_with('y')
        && bytes.len() >= 2
        && !matches!(
            bytes[bytes.len() - 2].to_ascii_lowercase(),
            b'a' | b'e' | b'i' | b'o' | b'u'
        )
    {
        return Some(format!("{}ies", &name[..name.len() - 1]));
    }
    if name.ends_with('s')
        || name.ends_with('x')
        || name.ends_with('z')
        || name.ends_with("ch")
        || name.ends_with("sh")
    {
        return Some(format!("{name}es"));
    }
    Some(format!("{name}s"))
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

/// Locals carrying an OOP "class" signal: the receiver of a `X.__index = ...`
/// assignment, the 2nd argument of `setmetatable(_, X)`, or the receiver of a
/// colon method-call `X:method(...)`. The `collect` table-arm combines this with
/// an empty-table declaration `local X = {}` to name the class table `class`.
///
/// Each signal alone is too broad (every instance is colon-called), so the
/// empty-table-decl gate is what makes the pair sound: a bare `{}` that is later
/// used as a metatable or colon-invoked can only be a class/object table.
fn collect_class_signals(block: &mut Block, out: &mut FxHashSet<usize>) {
    fn note_setmetatable(call: &Call, out: &mut FxHashSet<usize>) {
        if global_name(&call.value) == Some("setmetatable")
            && let Some(RValue::Local(meta)) = call.arguments.get(1)
        {
            out.insert(local_ptr(meta));
        }
    }
    fn note_colon_receiver(method_call: &MethodCall, out: &mut FxHashSet<usize>) {
        if let RValue::Local(receiver) = &*method_call.value {
            out.insert(local_ptr(receiver));
        }
    }

    for statement in &mut block.0 {
        // `X.__index = ...` — any assignment whose LHS indexes a local with the
        // key "__index" marks that local as a metatable/class.
        if let Statement::Assign(assign) = &*statement {
            for lvalue in &assign.left {
                if let LValue::Index(index) = lvalue
                    && let RValue::Local(base) = &*index.left
                    && index_key(index) == Some("__index")
                {
                    out.insert(local_ptr(base));
                }
            }
        }

        // Nested rvalues: `setmetatable(_, X)` calls and colon-calls `X:m()`.
        let mut functions = Vec::new();
        statement.post_traverse_values(&mut |value| -> Option<()> {
            match value {
                Either::Right(RValue::Closure(closure)) => {
                    functions.push(closure.function.clone());
                }
                Either::Right(RValue::Call(call))
                | Either::Right(RValue::Select(Select::Call(call))) => note_setmetatable(call, out),
                Either::Right(RValue::MethodCall(method_call))
                | Either::Right(RValue::Select(Select::MethodCall(method_call))) => {
                    note_colon_receiver(method_call, out)
                }
                _ => {}
            }
            None
        });
        // Top-level call / method-call statements are not exposed above.
        match &*statement {
            Statement::Call(call) => note_setmetatable(call, out),
            Statement::MethodCall(method_call) => note_colon_receiver(method_call, out),
            _ => {}
        }

        for function in functions {
            collect_class_signals(&mut function.lock().body, out);
        }
        match &*statement {
            Statement::If(r#if) => {
                collect_class_signals(&mut r#if.then_block.lock(), out);
                collect_class_signals(&mut r#if.else_block.lock(), out);
            }
            Statement::While(r#while) => collect_class_signals(&mut r#while.block.lock(), out),
            Statement::Repeat(repeat) => collect_class_signals(&mut repeat.block.lock(), out),
            Statement::NumericFor(numeric_for) => {
                collect_class_signals(&mut numeric_for.block.lock(), out)
            }
            Statement::GenericFor(generic_for) => {
                collect_class_signals(&mut generic_for.block.lock(), out)
            }
            _ => {}
        }
    }
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
    /// Homogeneous collection fills whose values are RBXScriptConnections.
    /// Unknown/mixed fills conflict so a table is never optimistically called
    /// `connections` after also storing unrelated values.
    connection_fills: u32,
    unknown_collection_fill: bool,
    /// Semantic sources written into an array/map container. Local sources are
    /// stored by address (never as `RcLocal`) so the census cannot perturb the
    /// Arc-count based unused-local test. They are resolved after the naming
    /// collector has seen the whole tree.
    collection_value_sources: FxHashSet<FillSource>,
    /// Semantic sources used as dynamic map keys (`map[key] = value`). These are
    /// optional: a unanimous value can still name a collection when its keys are
    /// opaque, while a unanimous key upgrades `models` to `modelsByPlayer`.
    collection_key_sources: FxHashSet<FillSource>,
    unknown_collection_key: bool,
    /// At least one fill had no trustworthy semantic source. A mixed/unknown
    /// collection is deliberately left at its existing name.
    unknown_semantic_fill: bool,
    /// Exact update-shape facts for otherwise mute numeric/boolean state.
    counter_updates: u32,
    counter_invalid_write: bool,
    boolean_writes: u32,
    boolean_invalid_write: bool,
    boolean_guarded: bool,
    /// Used as the saved side of `os.clock() - local` / `tick() - local`.
    elapsed_clock_base: bool,
    clock_writes: u32,
    clock_invalid_write: bool,
    /// A multi-result/vararg assignment may write this slot without an explicit
    /// one-to-one RHS node. Such a write invalidates every value-shape consensus.
    unknown_value_write: bool,
    /// Returned from its enclosing block/function.
    returned: bool,
    /// Stored as the value of a callback-shaped table field (`onClose = local`).
    callback_field_name: Option<String>,
    callback_name_conflict: bool,
    /// Implied type from a `typeof(x)`/`type(x)` guard, collapsed to a small
    /// stable tag (see [`type_tag`]). `None` until a guard is seen.
    typeof_type: Option<&'static str>,
    /// Two *different* type guards were seen on this local — it is genuinely
    /// polymorphic, so the namer refuses to name it from a type.
    typeof_conflict: bool,
    /// Used as the receiver of an instance-shaped method call (see
    /// [`INSTANCE_METHODS`]).
    instance_method_seen: bool,
    /// String-literal field KEY this local was stored INTO (`obj.Key = local`):
    /// a setter/ctor writes a param's value into a named field, so the field
    /// names the param (`self.range = p2` -> `range`). A direct dataflow fact,
    /// not a guess. `None` until a store is seen.
    field_store_key: Option<String>,
    /// The same local was stored into two *different* fields — ambiguous, so the
    /// namer refuses rather than pick one (mirrors `typeof_conflict`).
    field_store_conflict: bool,
    /// Used as the receiver of a string method (`local:gsub(...)`) — the local is
    /// a string (see [`STRING_METHODS`]).
    string_method_seen: bool,
    /// A known name-string API slot this local fills as an ARGUMENT
    /// (`x:FindFirstChild(local)` -> `"childName"`; `x:GetAttribute(local)` ->
    /// `"attributeName"`). `None` until such a use is seen.
    api_slot: Option<&'static str>,
    /// The local fills two *different* API slots — ambiguous, refuse.
    api_slot_conflict: bool,
    /// Literal attribute key this local sets (`x:SetAttribute("Key", local)` ->
    /// `"Key"`): the key is a source identifier naming the value. `None` until
    /// seen.
    attr_key: Option<String>,
    /// The local set two *different* attribute keys — ambiguous, refuse.
    attr_key_conflict: bool,
    /// Type implied by a `local or DEFAULT` fallback whose LEFT operand is this
    /// local (`number`/`string`/`table`). Reinforces a missing `typeof` guard.
    or_default_type: Option<&'static str>,
    /// Two *different* default types were seen — refuse.
    or_default_conflict: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum FillSource {
    Static(String),
    Local(usize),
}

fn note_callback_name(local: &RcLocal, name: String, usage: &mut FxHashMap<usize, LocalUsage>) {
    let entry = usage.entry(local_ptr(local)).or_default();
    match &entry.callback_field_name {
        None => entry.callback_field_name = Some(name),
        Some(existing) if existing != &name => entry.callback_name_conflict = true,
        _ => {}
    }
}

fn is_connection_value(value: &RValue) -> bool {
    match value {
        RValue::MethodCall(call) | RValue::Select(Select::MethodCall(call)) => {
            matches!(call.method.as_str(), "Connect" | "Once" | "ConnectParallel")
        }
        RValue::Table(table) if !table.0.is_empty() => {
            table.0.iter().all(|(_, value)| is_connection_value(value))
        }
        _ => false,
    }
}

fn table_entry_hint(table: &Table) -> Option<String> {
    let mut stem = None;
    for (key, _) in &table.0 {
        let Some(key) = key.as_ref().and_then(string_literal) else {
            continue;
        };
        let Some(prefix) = key.strip_suffix("Key").filter(|prefix| !prefix.is_empty()) else {
            continue;
        };
        let Some(candidate) = sanitize(prefix) else {
            continue;
        };
        match &stem {
            None => stem = Some(candidate),
            Some(existing) if existing != &candidate => return None,
            _ => {}
        }
    }
    stem.and_then(|stem| sanitize_preserve(&format!("{stem}Entry")))
}

fn fill_source(value: &RValue) -> Option<FillSource> {
    let source = match value {
        RValue::Local(local) => match current_name(local) {
            Some(name) if !is_generic_semantic_name(&name) => FillSource::Static(name),
            _ => FillSource::Local(local_ptr(local)),
        },
        RValue::Table(table) => FillSource::Static(table_entry_hint(table)?),
        _ => FillSource::Static(rvalue_hint(value)?),
    };
    match &source {
        FillSource::Static(name) if is_generic_semantic_name(name) => None,
        _ => Some(source),
    }
}

fn note_map_key(local: &RcLocal, key: &RValue, usage: &mut FxHashMap<usize, LocalUsage>) {
    let entry = usage.entry(local_ptr(local)).or_default();
    if let Some(source) = fill_source(key) {
        entry.collection_key_sources.insert(source);
    } else {
        entry.unknown_collection_key = true;
    }
}

fn note_collection_fill(local: &RcLocal, value: &RValue, usage: &mut FxHashMap<usize, LocalUsage>) {
    let entry = usage.entry(local_ptr(local)).or_default();
    if is_connection_value(value) {
        entry.connection_fills += 1;
        entry
            .collection_value_sources
            .insert(FillSource::Static("connection".to_string()));
    } else {
        entry.unknown_collection_fill = true;
        if let Some(source) = fill_source(value) {
            entry.collection_value_sources.insert(source);
        } else {
            entry.unknown_semantic_fill = true;
        }
    }
}

fn is_number(value: &RValue, expected: f64) -> bool {
    matches!(value, RValue::Literal(Literal::Number(number)) if *number == expected)
}

fn is_counter_update(local: &RcLocal, value: &RValue) -> bool {
    let RValue::Binary(binary) = value else {
        return false;
    };
    if binary.operation != BinaryOperation::Add {
        return false;
    }
    (matches!(&*binary.left, RValue::Local(source) if local_ptr(source) == local_ptr(local))
        && is_number(&binary.right, 1.0))
        || (is_number(&binary.left, 1.0)
            && matches!(&*binary.right, RValue::Local(source)
                if local_ptr(source) == local_ptr(local)))
}

fn note_local_write(local: &RcLocal, value: &RValue, usage: &mut FxHashMap<usize, LocalUsage>) {
    let entry = usage.entry(local_ptr(local)).or_default();
    if is_counter_update(local, value) {
        entry.counter_updates += 1;
    } else if !is_number(value, 0.0) {
        entry.counter_invalid_write = true;
    }

    match value {
        RValue::Literal(Literal::Boolean(_)) => entry.boolean_writes += 1,
        // A nil declaration is an uninitialized boolean cell, not evidence of a
        // second semantic type. Every concrete write still has to be boolean.
        RValue::Literal(Literal::Nil) => {}
        _ => entry.boolean_invalid_write = true,
    }

    if is_time_read(value) {
        entry.clock_writes += 1;
    } else if !matches!(value, RValue::Literal(Literal::Nil)) {
        entry.clock_invalid_write = true;
    }
}

fn note_unknown_local_write(local: &RcLocal, usage: &mut FxHashMap<usize, LocalUsage>) {
    let entry = usage.entry(local_ptr(local)).or_default();
    entry.counter_invalid_write = true;
    entry.boolean_invalid_write = true;
    entry.clock_invalid_write = true;
    entry.unknown_value_write = true;
}

fn is_time_read(value: &RValue) -> bool {
    let (RValue::Call(call) | RValue::Select(Select::Call(call))) = value else {
        return false;
    };
    if !call.arguments.is_empty() {
        return false;
    }
    match &*call.value {
        RValue::Global(global) => global.0.as_slice() == b"tick",
        RValue::Index(index) => {
            global_name(&index.left) == Some("os")
                && matches!(index_key(index), Some("clock") | Some("time"))
        }
        _ => false,
    }
}

fn note_elapsed_clock_base(binary: &Binary, usage: &mut FxHashMap<usize, LocalUsage>) {
    if binary.operation == BinaryOperation::Sub
        && is_time_read(&binary.left)
        && let RValue::Local(local) = &*binary.right
    {
        usage
            .entry(local_ptr(local))
            .or_default()
            .elapsed_clock_base = true;
    }
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
    if let RValue::Index(index) = &*call.value
        && index_key(index) == Some("insert")
        && let RValue::Global(global) = &*index.left
        && global.0.as_slice() == b"table"
        && let Some(RValue::Local(local)) = call.arguments.first()
    {
        if let Some(value) = call.arguments.get(1) {
            note_collection_fill(local, value, usage);
        }
        let pushes_element = call
            .arguments
            .get(1)
            .is_some_and(|value| is_create_element_call(value, aliases));
        let entry = usage.entry(local_ptr(local)).or_default();
        entry.table_insert_in_loop |= in_loop;
        // `table.insert(children, e(...))` in a loop is an array-style children map.
        if in_loop && pushes_element {
            entry.create_element_fill_count += 1;
            entry.create_element_fill_in_loop = true;
        }
    }

    // Stable standard-library argument slots (§2.C). These are positional API
    // contracts, not guesses from surrounding syntax, and disagreement is
    // handled by the same conflict-on-consensus gate as other slots.
    if let RValue::Index(callee) = &*call.value
        && let RValue::Global(namespace) = &*callee.left
        && let Some(member) = string_literal(&callee.right)
    {
        let namespace = namespace.0.as_slice();
        let slots: &[&str] = match (namespace, member) {
            (b"math", "clamp") => &["value", "min", "max"],
            (b"task", "wait") | (b"task", "delay") => &["duration"],
            (b"string", "format") => &["formatString"],
            _ => &[],
        };
        for (argument, slot) in call.arguments.iter().zip(slots) {
            if let RValue::Local(local) = argument {
                note_api_slot(local, slot, usage);
            }
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

/// String methods: a local used as the receiver of one of these is a string.
/// Kept strictly disjoint from [`INSTANCE_METHODS`] so the two never both fire on
/// one local (a string is not an Instance). Only the unambiguous `string` library
/// methods are listed.
const STRING_METHODS: &[&str] = &[
    "sub", "gsub", "gmatch", "match", "find", "lower", "upper", "split", "rep", "byte", "len",
    "format",
];

/// Instance lookups whose first ARGUMENT is a child NAME string. Restricted to the
/// two that genuinely take a name (`FindFirstChildOfClass`/`WhichIsA` take a class
/// name, not a child name, so they are excluded — naming their arg `childName`
/// would mislead).
const CHILD_LOOKUP_METHODS: &[&str] = &["FindFirstChild", "WaitForChild"];

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
        "CharacterAdded" | "CharacterRemoving" | "CharacterAppearanceLoaded" => {
            &[Some("character")]
        }
        "PlayerAdded" | "PlayerRemoving" => &[Some("player")],
        "Triggered" | "PromptTriggered" => &[Some("player")],
        "Touched" | "TouchEnded" => &[Some("otherPart")],
        _ => return None,
    })
}

/// Record method-call facts: receiver-side (instance/string shape) and
/// argument-side (a bare-local argument is named after the API slot it fills).
fn note_method_usage(method_call: &MethodCall, usage: &mut FxHashMap<usize, LocalUsage>) {
    let method = method_call.method.as_str();
    if let RValue::Local(local) = &*method_call.value {
        if INSTANCE_METHODS.contains(&method) {
            usage
                .entry(local_ptr(local))
                .or_default()
                .instance_method_seen = true;
        }
        if STRING_METHODS.contains(&method) {
            usage
                .entry(local_ptr(local))
                .or_default()
                .string_method_seen = true;
        }
    }
    if matches!(method, "Connect" | "Once" | "ConnectParallel")
        && let Some(RValue::Local(callback)) = method_call.arguments.first()
        && let RValue::Index(event) = &*method_call.value
        && let Some(event) = index_key(event)
    {
        let name = format!("on{}", capitalize_first(event));
        if let Some(name) = sanitize_preserve(&name) {
            note_callback_name(callback, name, usage);
        }
    }
    note_method_arg_usage(method_call, usage);
}

/// Name a bare-local ARGUMENT from the API slot it fills. The argument is a
/// *different* local from the receiver, so this never collides with the
/// receiver's instance hint. Only bare `RValue::Local` args qualify (a `p.Field`
/// or `p:Method()` arg is an `Index`/`MethodCall` node and is skipped).
fn note_method_arg_usage(method_call: &MethodCall, usage: &mut FxHashMap<usize, LocalUsage>) {
    let method = method_call.method.as_str();
    let args = &method_call.arguments;
    if CHILD_LOOKUP_METHODS.contains(&method)
        && let Some(RValue::Local(arg)) = args.first()
    {
        note_api_slot(arg, "childName", usage);
    }
    if method == "GetAttribute"
        && args.len() == 1
        && let Some(RValue::Local(arg)) = args.first()
    {
        note_api_slot(arg, "attributeName", usage);
    }
    // `x:SetAttribute("Key", local)` — the literal key is a source identifier
    // naming the value being written.
    if method == "SetAttribute"
        && let Some(key) = args.first().and_then(string_literal)
        && let Some(RValue::Local(arg)) = args.get(1)
    {
        let entry = usage.entry(local_ptr(arg)).or_default();
        match &entry.attr_key {
            None => entry.attr_key = Some(key.to_string()),
            Some(existing) if existing != key => entry.attr_key_conflict = true,
            _ => {}
        }
    }
}

/// Record (with conflict-on-disagreement) that `arg` fills the given API slot.
fn note_api_slot(arg: &RcLocal, slot: &'static str, usage: &mut FxHashMap<usize, LocalUsage>) {
    let entry = usage.entry(local_ptr(arg)).or_default();
    match entry.api_slot {
        None => entry.api_slot = Some(slot),
        Some(existing) if existing != slot => entry.api_slot_conflict = true,
        _ => {}
    }
}

/// Record that a param's value was stored into a named field (`obj.Key = param`):
/// the destination key names the param. Only a bare `RValue::Local` RHS qualifies
/// — a wrapped RHS (`obj.CFrame = CFrame.new(p5)`, `obj.X = f(p)`) is a
/// `Call`/`Index` node, so `p5`/`p` is correctly skipped (it is not *the* value of
/// that field). A string-literal key only (dynamic `t[i] = p` is excluded). Two
/// distinct destination keys flag a conflict (refuse, mirroring `note_type_guard`).
fn note_field_store(lvalue: &LValue, rvalue: &RValue, usage: &mut FxHashMap<usize, LocalUsage>) {
    let LValue::Index(index) = lvalue else {
        return;
    };
    let Some(key) = string_literal(&index.right) else {
        return;
    };
    let RValue::Local(local) = rvalue else {
        return;
    };
    let entry = usage.entry(local_ptr(local)).or_default();
    match &entry.field_store_key {
        None => entry.field_store_key = Some(key.to_string()),
        Some(existing) if existing != key => entry.field_store_conflict = true,
        _ => {}
    }
}

/// Record the implied type of a `param or DEFAULT` fallback (LEFT operand is the
/// param). Only literal/empty-table defaults are classified — they are
/// unambiguous. Two different default types flag a conflict (refuse).
fn note_or_default(binary: &Binary, usage: &mut FxHashMap<usize, LocalUsage>) {
    if binary.operation != BinaryOperation::Or {
        return;
    }
    let RValue::Local(local) = &*binary.left else {
        return;
    };
    let tag = match &*binary.right {
        RValue::Literal(Literal::Number(_)) => "number",
        RValue::Literal(Literal::String(_)) => "string",
        RValue::Table(table) if table.0.is_empty() => "table",
        _ => return,
    };
    let entry = usage.entry(local_ptr(local)).or_default();
    match entry.or_default_type {
        None => entry.or_default_type = Some(tag),
        Some(existing) if existing != tag => entry.or_default_conflict = true,
        _ => {}
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
                Either::Right(RValue::Call(call))
                | Either::Right(RValue::Select(Select::Call(call))) => {
                    note_call_usage(call, in_loop, aliases, usage)
                }
                Either::Right(RValue::Table(table)) => {
                    for (key, val) in &table.0 {
                        if let (Some(key), RValue::Local(local)) = (key.as_ref(), val)
                            && let Some(key) = string_literal(key)
                            && is_callback_key(key)
                        {
                            note_callback_name(local, key.to_string(), usage);
                        }
                    }
                }
                Either::Right(RValue::MethodCall(method_call))
                | Either::Right(RValue::Select(Select::MethodCall(method_call))) => {
                    note_method_usage(method_call, usage);
                }
                Either::Right(RValue::Binary(binary)) => {
                    note_type_guard(binary, usage);
                    note_or_default(binary, usage);
                    note_elapsed_clock_base(binary, usage);
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
                    note_field_store(lvalue, rvalue, usage);
                    if let LValue::Local(local) = lvalue {
                        note_local_write(local, rvalue, usage);
                    }
                    if let crate::LValue::Index(index) = lvalue
                        && let RValue::Local(local) = &*index.left
                    {
                        if string_literal(&index.right).is_none() {
                            note_collection_fill(local, rvalue, usage);
                            note_map_key(local, &index.right, usage);
                        }
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
                if assign
                    .right
                    .last()
                    .is_some_and(|value| matches!(value, RValue::Select(_) | RValue::VarArg(_)))
                {
                    for lvalue in assign.left.iter().skip(assign.right.len()) {
                        if let LValue::Local(local) = lvalue {
                            note_unknown_local_write(local, usage);
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
            Statement::If(node) => {
                if let RValue::Local(local) = &node.condition {
                    usage.entry(local_ptr(local)).or_default().boolean_guarded = true;
                }
            }
            Statement::While(node) => {
                if let RValue::Local(local) = &node.condition {
                    usage.entry(local_ptr(local)).or_default().boolean_guarded = true;
                }
            }
            Statement::Repeat(node) => {
                if let RValue::Local(local) = &node.condition {
                    usage.entry(local_ptr(local)).or_default().boolean_guarded = true;
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
            Statement::While(r#while) => {
                gather_usage(&mut r#while.block.lock(), true, aliases, usage)
            }
            Statement::Repeat(repeat) => {
                gather_usage(&mut repeat.block.lock(), true, aliases, usage)
            }
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
            Statement::Repeat(repeat) => collect_collapse_candidates(&mut repeat.block.lock(), out),
            Statement::NumericFor(nf) => collect_collapse_candidates(&mut nf.block.lock(), out),
            Statement::GenericFor(gf) => collect_collapse_candidates(&mut gf.block.lock(), out),
            _ => {}
        }
    }
}

struct Namer {
    rename: bool,
    dont_reuse_var: bool,
    module_hint: Option<String>,
    /// Names that may not currently be assigned to a local: every global
    /// referenced in the program (added once in `collect`, kept reserved
    /// program-wide so a referenced global is never shadowed) plus the names of
    /// every local that is CURRENTLY in scope. Lexically scoped names are added
    /// when a scope is entered and released (via `release`) when it ends, so two
    /// locals in disjoint/sibling scopes may reuse the same base name while two
    /// simultaneously-visible locals never collide.
    reserved: FxHashSet<String>,
    /// Names already assigned in this file when `dont_reuse_var` is enabled.
    /// Regular locals consult this set. Loop header locals add to it, but do not
    /// consult it, so sibling `for i` / `for k, v` loops stay idiomatic while
    /// later regular locals still avoid those names.
    used_file_names: FxHashSet<String>,
    /// Next suffix to try for a base in `dont_reuse_var` mode, avoiding repeated
    /// scans through `v2`, `v3`, ... in large files.
    next_file_suffix: FxHashMap<String, usize>,
    /// Preferred base name for a local, keyed by `local_ptr`.
    hints: FxHashMap<usize, Hint>,
    /// Broader context names that are safer than a narrow type name when usage
    /// proves the same local may hold several unrelated Instance classes.
    context_hints: FxHashMap<usize, String>,
    /// Class families observed through `:IsA(...)`, used to avoid naming a
    /// mixed-class local after only the first branch that inspected it.
    isa_families: FxHashMap<usize, String>,
    isa_conflicts: FxHashSet<usize>,
    isa_derived_hints: FxHashMap<usize, String>,
    /// Exact `Instance.new("Class")` assignment consensus. This is gathered
    /// across all writes before any class hint is applied, so branch traversal
    /// order cannot choose a misleading first class.
    instance_assignment_hints: FxHashMap<usize, String>,
    instance_assignment_conflicts: FxHashSet<usize>,
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
    /// Locals carrying an OOP "class" signal (`X.__index = ..`,
    /// `setmetatable(_, X)`, or a colon-call `X:m()`). Combined with an empty-
    /// table declaration `local X = {}` to name the class table `class`.
    /// See `collect_class_signals`.
    class_signal_locals: FxHashSet<usize>,
}

struct ParamConsensus {
    calls: usize,
    names: Vec<Option<String>>,
    valid: Vec<bool>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ReusePolicy {
    FileUnique,
    LoopReusable,
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

    fn set_hint_ptr(&mut self, ptr: usize, name: String, score: u8) {
        let replace = match self.hints.get(&ptr) {
            Some(existing) => score > existing.score,
            None => true,
        };
        if replace {
            self.hints.insert(ptr, Hint { name, score });
        }
    }

    fn set_hint(&mut self, local: &RcLocal, name: String, score: u8) {
        self.set_hint_ptr(local_ptr(local), name, score);
    }

    fn set_hint_str(&mut self, local: &RcLocal, name: &'static str, score: u8) {
        self.set_hint(local, name.to_string(), score);
    }

    fn set_context_hint_str(&mut self, local: &RcLocal, name: &'static str, score: u8) {
        self.context_hints
            .insert(local_ptr(local), name.to_string());
        self.set_hint_str(local, name, score);
    }

    fn set_isa_derived_hint(&mut self, local: &RcLocal, name: String, score: u8) {
        let ptr = local_ptr(local);
        self.set_hint_ptr(ptr, name.clone(), score);
        if self
            .hints
            .get(&ptr)
            .is_some_and(|hint| hint.score == score && hint.name == name)
        {
            self.isa_derived_hints.insert(ptr, name);
        }
    }

    fn note_instance_assignment(&mut self, local: &RcLocal, name: String) {
        let ptr = local_ptr(local);
        match self.instance_assignment_hints.get(&ptr) {
            None => {
                self.instance_assignment_hints.insert(ptr, name);
            }
            Some(existing) if existing != &name => {
                self.instance_assignment_conflicts.insert(ptr);
            }
            _ => {}
        }
    }

    fn set_isa_hint(&mut self, local: &RcLocal, class_name: &str) {
        let Some(hint) = class_name_hint(class_name) else {
            return;
        };

        let ptr = local_ptr(local);
        let family = class_hint_family(class_name);
        if self.isa_conflicts.contains(&ptr) {
            return;
        }
        if let Some(existing_family) = self.isa_families.get(&ptr) {
            if existing_family != &family {
                self.isa_conflicts.insert(ptr);
                if let Some(derived) = self.isa_derived_hints.remove(&ptr)
                    && self
                        .hints
                        .get(&ptr)
                        .is_some_and(|hint| hint.score <= 56 && hint.name == derived)
                {
                    self.hints.remove(&ptr);
                }
                if let Some(fallback) = self.context_hints.get(&ptr).cloned() {
                    self.set_hint(local, fallback, 56);
                }
                return;
            }
            if self
                .isa_derived_hints
                .get(&ptr)
                .is_some_and(|existing| existing != &hint)
            {
                // Several concrete classes from one semantic family should use
                // the honest family name (`ParticleEmitter|Beam` -> `effect`),
                // never whichever `:IsA` happened to be visited first.
                self.set_isa_derived_hint(local, family.clone(), 56);
                return;
            }
        } else {
            self.isa_families.insert(ptr, family);
        }

        self.set_isa_derived_hint(local, hint, 55);
    }

    fn hint_name(&self, local: &RcLocal) -> Option<&str> {
        self.hints
            .get(&local_ptr(local))
            .map(|hint| hint.name.as_str())
    }

    fn local_known_name(&self, local: &RcLocal) -> Option<String> {
        current_name(local).or_else(|| self.hint_name(local).map(str::to_string))
    }

    fn resolve_fill_source(&self, source: &FillSource) -> Option<String> {
        let name = match source {
            FillSource::Static(name) => name.clone(),
            FillSource::Local(ptr) => self.hints.get(ptr)?.name.clone(),
        };
        (!is_generic_semantic_name(&name)).then_some(name)
    }

    fn unanimous_fill_name(&self, sources: &FxHashSet<FillSource>) -> Option<String> {
        let mut result = None;
        for source in sources {
            let name = self.resolve_fill_source(source)?;
            match &result {
                None => result = Some(name),
                Some(existing) if existing != &name => return None,
                _ => {}
            }
        }
        result
    }

    /// Apply whole-tree state/container facts after `collect` has populated every
    /// RHS-derived hint. Deferring this step lets `map[k] = valueLocal` resolve
    /// `valueLocal` even when its declaration appears later in traversal order.
    fn usage_based_hints(&mut self) {
        let mut candidates = Vec::new();
        let ambiguous_instances: Vec<usize> = self
            .instance_assignment_hints
            .keys()
            .copied()
            .filter(|ptr| self.instance_assignment_conflicts.contains(ptr))
            .collect();
        for ptr in ambiguous_instances {
            // A lookup-derived class word (score 60) is no more trustworthy than
            // the conflicting constructor class. Remove it instead of letting
            // traversal order choose either concrete class.
            if self.hints.get(&ptr).is_some_and(|hint| hint.score <= 60) {
                self.hints.remove(&ptr);
            }
        }
        for (&ptr, name) in &self.instance_assignment_hints {
            if !self.instance_assignment_conflicts.contains(&ptr)
                && !self.collapse_candidates.contains(&ptr)
                && !self
                    .usage
                    .get(&ptr)
                    .is_some_and(|usage| usage.unknown_value_write)
            {
                candidates.push((ptr, name.clone(), 65));
            }
        }
        for (&ptr, usage) in &self.usage {
            if self.collapse_candidates.contains(&ptr) {
                continue;
            }

            if !usage.unknown_semantic_fill
                && let Some(value) = self.unanimous_fill_name(&usage.collection_value_sources)
                && let Some(values) = pluralize(&value)
            {
                let key = (!usage.unknown_collection_key)
                    .then(|| self.unanimous_fill_name(&usage.collection_key_sources))
                    .flatten();
                let (name, score) = match key {
                    Some(key) if !name_ends_with_word(&values, &key) => {
                        (format!("{values}By{}", capitalize_first(&key)), 53)
                    }
                    _ => (values, 52),
                };
                if let Some(name) = sanitize_preserve(&name) {
                    candidates.push((ptr, name, score));
                }
            }
            if usage.counter_updates > 0 && !usage.counter_invalid_write {
                candidates.push((ptr, "count".to_string(), 46));
            }
            if usage.boolean_writes > 0 && usage.boolean_guarded && !usage.boolean_invalid_write {
                candidates.push((ptr, "flag".to_string(), 38));
            }
            if usage.elapsed_clock_base && usage.clock_writes > 0 && !usage.clock_invalid_write {
                candidates.push((ptr, "lastTime".to_string(), 61));
            }
        }
        for (ptr, name, score) in candidates {
            self.set_hint_ptr(ptr, name, score);
        }
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

    /// A local bound to a factory/getter call reads as the call's subject:
    /// `local v = getOwnPlot()` -> `ownPlot`, `local v = createButton(...)` ->
    /// `button`, `local v = getOrCreateFXPart(...)` -> `fxPart`. The callee is
    /// resolved exactly as `predicate_call_hint` does (`callable_name` — a local
    /// function, a global, or an indexed method), then an allow-listed verb prefix
    /// is stripped (see `strip_verb_prefix`). The RHS is a `Call` (non-movable,
    /// `is_movable_single_value` = false), so the local is a guaranteed survivor of
    /// the inline/collapse passes — naming it can never suppress one (+lines-safe).
    fn verb_call_hint(&self, rvalue: &RValue) -> Option<String> {
        let (RValue::Call(call) | RValue::Select(Select::Call(call))) = rvalue else {
            return None;
        };
        let name = self.callable_name(&call.value)?;
        sanitize(strip_verb_prefix(&name)?)
    }

    /// A stored `TweenService:Create(...)` result reads as `tween` (the near-
    /// universal source name). Gated on the receiver actually being TweenService —
    /// `method == "Create"` alone is ambiguous (a custom class can define a
    /// `:Create()` constructor, e.g. `EmiliaFBXTalkFX:Create()`, which must NOT be
    /// named `tween`).
    fn tween_create_hint(&self, rvalue: &RValue) -> Option<String> {
        let (RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call))) =
            rvalue
        else {
            return None;
        };
        if method_call.method != "Create" {
            return None;
        }
        self.is_tween_service(&method_call.value)
            .then(|| "tween".to_string())
    }

    /// Whether `receiver` denotes the TweenService — either a local resolving to
    /// the name `TweenService` (the GetService-preserved header local) or the
    /// inline `_:GetService("TweenService")` call.
    fn is_tween_service(&self, receiver: &RValue) -> bool {
        match receiver {
            RValue::Local(local) => self.local_known_name(local).as_deref() == Some("TweenService"),
            RValue::MethodCall(method_call) | RValue::Select(Select::MethodCall(method_call)) => {
                method_call.method == "GetService"
                    && method_call.arguments.first().and_then(string_literal)
                        == Some("TweenService")
            }
            _ => false,
        }
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
        let (children, result, connections) = match self.usage.get(&local_ptr(local)) {
            Some(usage) => (
                usage.create_element_fill_count >= 2 || usage.create_element_fill_in_loop,
                (usage.keyed_assign_in_loop || usage.table_insert_in_loop) && usage.returned,
                usage.connection_fills > 0 && !usage.unknown_collection_fill,
            ),
            None => (false, false, false),
        };
        if children {
            self.set_hint_str(local, "children", 48);
        } else if connections {
            self.set_hint_str(local, "connections", 52);
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
        let key = self.usage.get(&local_ptr(local)).and_then(|usage| {
            (!usage.callback_name_conflict)
                .then(|| usage.callback_field_name.clone())
                .flatten()
        });
        if let Some(key) = key
            && let Some(name) = sanitize_preserve(&key)
        {
            self.set_hint(local, name, 62);
        }
    }

    fn callsite_argument_name(&self, value: &RValue) -> Option<String> {
        let name = match value {
            RValue::Local(local) => self.local_known_name(local),
            RValue::Index(index) => index_key(index).and_then(sanitize),
            _ => rvalue_hint(value),
        }?;
        (!is_default_name(&name) && name != "_").then_some(name)
    }

    /// Name local-function parameters from unanimous semantic call-site sources
    /// (`render(petData)` / `render(record.PetData)` -> `petData`). Definitions
    /// and calls are collected in separate whole-tree walks so declaration order
    /// is irrelevant. Any unknown or disagreeing site invalidates that slot.
    fn interprocedural_param_hints(&mut self, block: &Block) {
        let mut definitions = FxHashMap::<usize, Vec<RcLocal>>::default();
        let mut invalid = FxHashSet::default();
        collect_local_function_definitions(block, &mut definitions, &mut invalid);
        definitions.retain(|binder, _| !invalid.contains(binder));
        if definitions.is_empty() {
            return;
        }

        let mut consensus = FxHashMap::<usize, ParamConsensus>::default();
        collect_local_function_calls(block, &definitions, self, &mut consensus);
        let mut hints = Vec::new();
        for (binder, state) in consensus {
            if state.calls == 0 {
                continue;
            }
            let Some(parameters) = definitions.get(&binder) else {
                continue;
            };
            for (index, parameter) in parameters.iter().enumerate() {
                if state.valid.get(index) == Some(&true)
                    && let Some(name) = state.names.get(index).and_then(Clone::clone)
                {
                    hints.push((parameter.clone(), name));
                }
            }
        }
        // Drop all temporary RcLocal clones before `apply` performs Arc-count-
        // based unused detection; only pointer-keyed hints remain in `self`.
        drop(definitions);
        for (parameter, name) in hints {
            self.set_hint(&parameter, name, 49);
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
        // `.UserId` alone is common on plain data records and must not turn a
        // pet/user entry into `player`. `.Character` is Player-specific; the
        // UserId+DisplayName pair is also strong enough when Character is absent.
        let is_player = usage.string_fields_read.contains("Character")
            || (usage.string_fields_read.contains("UserId")
                && usage.string_fields_read.contains("DisplayName"));
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

    /// Names a param from DATAFLOW facts about how its value is used (§param
    /// naming v1). Unlike `usage_param_hint` (a single early-return ladder of
    /// type GUESSES), these are independent and arbitrated purely by score via
    /// `set_hint`'s strict `>`, so a stronger signal always wins regardless of
    /// call order. All are intra-function and `+lines`-safe (a param is never an
    /// inliner candidate). Scores: a written source token (field key 48, attr key
    /// 47) outranks the type-hypernym band (api name-string 43, or-default options
    /// 41 / value 40, string-method value 40); the weakest GUESS (callee 37) sits
    /// below every existing param hint so it only fills an otherwise-`p` slot.
    fn param_dataflow_hint(&mut self, param: &RcLocal) {
        let Some(usage) = self.usage.get(&local_ptr(param)) else {
            return;
        };
        let instance_shaped = usage.instance_method_seen;
        let typeof_type = usage.typeof_type;
        let is_instance_typeof = typeof_type == Some("Instance");

        // A destination field key names the value written into it (dataflow fact).
        let field_name = if usage.field_store_conflict {
            None
        } else {
            usage
                .field_store_key
                .as_deref()
                .and_then(param_name_from_field_key)
        };
        // A literal attribute key is a source identifier naming the value — same
        // shape as a field key, so it strips `_`/trailing digits and refuses
        // generic keys identically (shared helper).
        let attr_name = if usage.attr_key_conflict {
            None
        } else {
            usage
                .attr_key
                .as_deref()
                .and_then(param_name_from_field_key)
        };
        // A name-string API slot — but a name string can't be an Instance, so a
        // contradicting instance use refuses it.
        let api_name = if usage.api_slot_conflict || instance_shaped || is_instance_typeof {
            None
        } else {
            usage.api_slot
        };
        // A string-method receiver is a string (=> the `value` hypernym, matching
        // the typeof-string tier). Refuse on a contradicting Instance use.
        let string_value = usage.string_method_seen && !instance_shaped && !is_instance_typeof;
        // A literal/empty-table default reveals a scalar/table type. Refuse on a
        // contradicting Instance use (an Instance param can't default to 0/{}).
        let or_default = if usage.or_default_conflict || instance_shaped || is_instance_typeof {
            None
        } else {
            usage.or_default_type
        };
        // An invoked param is a callback — the weakest GUESS, refused by any
        // contradicting scalar/Instance evidence.
        let callee = usage.used_as_callee
            && !instance_shaped
            && !matches!(
                typeof_type,
                Some("string") | Some("number") | Some("Instance")
            );

        // The immutable `usage` borrow ends here; the `set_hint` calls take
        // `&mut self`. Scores arbitrate — order below is immaterial.
        if let Some(name) = field_name {
            self.set_hint(param, name, 48);
        }
        if let Some(name) = attr_name {
            self.set_hint(param, name, 47);
        }
        if let Some(slot) = api_name {
            self.set_hint_str(param, slot, 43);
        }
        if string_value {
            self.set_hint_str(param, "value", 40);
        }
        match or_default {
            Some("number") | Some("string") => self.set_hint_str(param, "value", 40),
            Some("table") => self.set_hint_str(param, "options", 41),
            _ => {}
        }
        if callee {
            self.set_hint_str(param, "callback", 37);
        }
    }

    /// Apply a name-string API-slot fact to a non-parameter local. The usage
    /// census is whole-tree, so this works even when the use occurs later or in
    /// a nested branch. As with parameter dataflow, contradictory Instance use,
    /// an Instance type guard, or disagreement between API slots suppresses the
    /// hint rather than producing a confident but false name.
    fn api_slot_local_hint(&mut self, local: &RcLocal) {
        let slot = self.usage.get(&local_ptr(local)).and_then(|usage| {
            (!usage.api_slot_conflict
                && !usage.typeof_conflict
                && !usage.instance_method_seen
                && matches!(usage.typeof_type, None | Some("string")))
            .then_some(usage.api_slot)
            .flatten()
        });
        if let Some(slot) = slot {
            // A positional API contract is stronger than the collection-name
            // singularization tier (47): `names` may describe what the strings
            // refer to, while `childName` describes what each value actually is.
            self.set_hint_str(local, slot, 50);
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
    fn name_is_taken(&self, name: &str, policy: ReusePolicy) -> bool {
        self.reserved.contains(name)
            || (self.dont_reuse_var
                && policy == ReusePolicy::FileUnique
                && self.used_file_names.contains(name))
    }

    fn unique(&mut self, base: &str, scope: &mut Vec<String>, policy: ReusePolicy) -> String {
        let name = if !self.name_is_taken(base, policy) {
            base.to_string()
        } else if self.dont_reuse_var && policy == ReusePolicy::FileUnique {
            let mut counter = self.next_file_suffix.get(base).copied().unwrap_or(2);
            loop {
                let candidate = format!("{}{}", base, counter);
                counter += 1;
                if !self.name_is_taken(&candidate, policy) {
                    self.next_file_suffix.insert(base.to_string(), counter);
                    break candidate;
                }
            }
        } else {
            let mut counter = 2;
            loop {
                let candidate = format!("{}{}", base, counter);
                if !self.name_is_taken(&candidate, policy) {
                    break candidate;
                }
                counter += 1;
            }
        };
        self.reserved.insert(name.clone());
        if self.dont_reuse_var {
            self.used_file_names.insert(name.clone());
        }
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

    fn name_one(
        &mut self,
        local: &RcLocal,
        default_prefix: &str,
        scope: &mut Vec<String>,
        policy: ReusePolicy,
    ) {
        let ptr = local_ptr(local);
        let mut lock = local.0 .0.lock();
        if !self.named.insert(ptr) {
            if let Some(name) = &lock.0
                && name != "_"
                && self.reserved.insert(name.clone())
            {
                if self.dont_reuse_var {
                    self.used_file_names.insert(name.clone());
                }
                scope.push(name.clone());
            }
            return;
        }
        if let Some(name) = lock.0.clone()
            && is_constant_identifier(&name)
        {
            lock.0 = Some(self.unique(&name, scope, policy));
            return;
        }
        // Late irreducible-control-flow dispatchers deliberately carry semantic
        // names.  They are minted after the normal hint-gathering phase, so
        // renaming them through the default path would degrade the rare fallback
        // to opaque `vN` state variables.  Preserve the readable base while still
        // applying normal collision handling across nested dispatchers/user names.
        if let Some(name) = lock.0.clone()
            && matches!(
                name.as_str(),
                "controlFlowState" | "controlFlowJumped" | "controlFlowExit"
            )
        {
            lock.0 = Some(self.unique(&name, scope, policy));
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
                lock.0 = Some(self.unique(&hint, scope, policy));
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
        lock.0 = Some(self.unique(&base, scope, policy));
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
                    self.param_dataflow_hint(param);
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
                            self.children_or_result_hint(local);
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
                                // An EMPTY table later used as a metatable
                                // (`X.__index = X` / `setmetatable(_, X)`) or colon-
                                // invoked (`X:method()`) is an OOP class table; name
                                // it `class`. The empty-table gate is what makes the
                                // (otherwise broad) class signals sound — a bare `{}`
                                // that is metatable-/colon-used can only be a class.
                                // Score 36 is far below the module name (100) and
                                // collection (90), so a returned module class keeps
                                // its script-derived name.
                                //
                                // NOTE: an empty `{}` IS `is_movable_single_value`
                                // (vacuously — `.all()` over no entries), so a SINGLE-
                                // USE empty-table temp WOULD be folded by the later
                                // `inline_single_use_temps` (-1 line). Naming it `class`
                                // makes `is_generated_temp` false and suppresses that
                                // inline (+1 line). So additionally gate on it NOT being
                                // that inline shape (`reads == 1 && writes == 1 &&
                                // !captured`); a genuine class table is referenced by
                                // its metatable / method defs / return, hence always
                                // multi-use, so this never drops a real class (all 3
                                // corpus sites keep `class`).
                                let inlinable_temp = self
                                    .counts
                                    .get(&local_ptr(local))
                                    .is_some_and(|u| u.reads == 1 && u.writes == 1 && !u.captured);
                                if table.0.is_empty()
                                    && !inlinable_temp
                                    && self.class_signal_locals.contains(&local_ptr(local))
                                {
                                    self.set_hint_str(local, "class", 36);
                                }
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
                                if let Some(hint) = instance_constructor_hint(rvalue) {
                                    self.note_instance_assignment(local, hint);
                                } else {
                                    if !is_instance_compatible_placeholder(rvalue) {
                                        self.instance_assignment_conflicts.insert(local_ptr(local));
                                    }
                                    if let Some(hint) = rvalue_hint(rvalue) {
                                        self.set_hint(local, hint, 60);
                                    }
                                    // A dynamic FindFirstChild/WaitForChild result
                                    // has only the honest hypernym `child`. Keep it
                                    // below concrete body/type evidence (`IsA`,
                                    // collection context), and out of rvalue_hint
                                    // so nested require paths cannot leak it onto
                                    // the module export local.
                                    if let Some(hint) = dynamic_child_lookup_rvalue_hint(rvalue) {
                                        self.set_hint(local, hint, 50);
                                    }
                                }
                                if let RValue::Closure(closure) = rvalue
                                    && let Some(name) = closure
                                        .function
                                        .lock()
                                        .name
                                        .as_deref()
                                        .and_then(sanitize_preserve)
                                {
                                    self.set_hint(local, name, 80);
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
                                // Factory/getter call result -> its subject
                                // (`local v = getOwnPlot()` -> `ownPlot`). Score 60 =
                                // rvalue_hint tier; `call_hint` returns nothing for a
                                // local-function callee, so this fills the empty slot.
                                if let Some(name) = self.verb_call_hint(rvalue) {
                                    self.set_hint(local, name, 60);
                                }
                                // `TweenService:Create(...)` result -> `tween`
                                // (`method_call_hint` returns nothing for `:Create`).
                                if let Some(name) = self.tween_create_hint(rvalue) {
                                    self.set_hint(local, name, 60);
                                }
                            }
                            self.ref_hint(local, rvalue);
                            // Generic guarded-lookup children get parent-qualified
                            // (`plantedSeedsClient`) rather than colliding to
                            // `client`/`client2`.
                            if let Some((name, score)) = self.guarded_lookup_qualified_hint(rvalue)
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
                        // Loop binders are not function parameters, but they can
                        // carry the same exact API-slot dataflow. In particular,
                        // `for _, name in ipairs({...}); FindFirstChild(name)`
                        // should expose that the iterated string is a child name.
                        self.api_slot_local_hint(res_local);
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
                        self.name_one(local, "v", &mut block_scope, ReusePolicy::FileUnique);
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
                    self.name_one(param, "p", &mut param_scope, ReusePolicy::FileUnique);
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
                    self.name_one(
                        &numeric_for.counter,
                        "v",
                        &mut loop_scope,
                        ReusePolicy::LoopReusable,
                    );
                    self.apply(&mut numeric_for.block.lock());
                    self.release(loop_scope);
                }
                Statement::GenericFor(generic_for) => {
                    let mut loop_scope: Vec<String> = Vec::new();
                    for res_local in &generic_for.res_locals {
                        self.name_one(res_local, "v", &mut loop_scope, ReusePolicy::LoopReusable);
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

fn collect_local_function_definitions(
    block: &Block,
    definitions: &mut FxHashMap<usize, Vec<RcLocal>>,
    invalid: &mut FxHashSet<usize>,
) {
    for statement in &block.0 {
        if let Statement::Assign(assign) = statement {
            let multi_tail = assign
                .right
                .last()
                .is_some_and(|value| matches!(value, RValue::Select(_) | RValue::VarArg(_)));
            for (index, left) in assign.left.iter().enumerate() {
                let LValue::Local(binder) = left else {
                    continue;
                };
                let ptr = local_ptr(binder);
                match assign.right.get(index) {
                    Some(RValue::Closure(closure)) => {
                        let parameters = closure.function.lock().parameters.clone();
                        if definitions.insert(ptr, parameters).is_some() {
                            invalid.insert(ptr);
                        }
                    }
                    Some(RValue::Literal(Literal::Nil)) if assign.prefix => {}
                    None if assign.prefix && !multi_tail => {}
                    _ => {
                        invalid.insert(ptr);
                    }
                }
            }
        }
        for value in crate::deinline::stmt_rvalues(statement) {
            collect_definitions_in_rvalue(value, definitions, invalid);
        }
        match statement {
            Statement::If(node) => {
                collect_local_function_definitions(&node.then_block.lock(), definitions, invalid);
                collect_local_function_definitions(&node.else_block.lock(), definitions, invalid);
            }
            Statement::While(node) => {
                collect_local_function_definitions(&node.block.lock(), definitions, invalid)
            }
            Statement::Repeat(node) => {
                collect_local_function_definitions(&node.block.lock(), definitions, invalid)
            }
            Statement::NumericFor(node) => {
                collect_local_function_definitions(&node.block.lock(), definitions, invalid)
            }
            Statement::GenericFor(node) => {
                collect_local_function_definitions(&node.block.lock(), definitions, invalid)
            }
            _ => {}
        }
    }
}

fn collect_definitions_in_rvalue(
    value: &RValue,
    definitions: &mut FxHashMap<usize, Vec<RcLocal>>,
    invalid: &mut FxHashSet<usize>,
) {
    if let RValue::Closure(closure) = value {
        collect_local_function_definitions(&closure.function.lock().body, definitions, invalid);
        return;
    }
    for child in value.rvalues() {
        collect_definitions_in_rvalue(child, definitions, invalid);
    }
}

fn record_local_function_call(
    call: &Call,
    definitions: &FxHashMap<usize, Vec<RcLocal>>,
    namer: &Namer,
    consensus: &mut FxHashMap<usize, ParamConsensus>,
) {
    let RValue::Local(binder) = &*call.value else {
        return;
    };
    let ptr = local_ptr(binder);
    let Some(parameters) = definitions.get(&ptr) else {
        return;
    };
    let state = consensus.entry(ptr).or_insert_with(|| ParamConsensus {
        calls: 0,
        names: vec![None; parameters.len()],
        valid: vec![true; parameters.len()],
    });
    state.calls += 1;
    for index in 0..parameters.len() {
        let Some(name) = call
            .arguments
            .get(index)
            .and_then(|argument| namer.callsite_argument_name(argument))
        else {
            state.valid[index] = false;
            continue;
        };
        match &state.names[index] {
            None => state.names[index] = Some(name),
            Some(existing) if existing != &name => state.valid[index] = false,
            _ => {}
        }
    }
}

fn collect_local_function_calls(
    block: &Block,
    definitions: &FxHashMap<usize, Vec<RcLocal>>,
    namer: &Namer,
    consensus: &mut FxHashMap<usize, ParamConsensus>,
) {
    for statement in &block.0 {
        if let Statement::Call(call) = statement {
            record_local_function_call(call, definitions, namer, consensus);
        }
        for value in crate::deinline::stmt_rvalues(statement) {
            collect_calls_in_rvalue(value, definitions, namer, consensus);
        }
        match statement {
            Statement::If(node) => {
                collect_local_function_calls(
                    &node.then_block.lock(),
                    definitions,
                    namer,
                    consensus,
                );
                collect_local_function_calls(
                    &node.else_block.lock(),
                    definitions,
                    namer,
                    consensus,
                );
            }
            Statement::While(node) => {
                collect_local_function_calls(&node.block.lock(), definitions, namer, consensus)
            }
            Statement::Repeat(node) => {
                collect_local_function_calls(&node.block.lock(), definitions, namer, consensus)
            }
            Statement::NumericFor(node) => {
                collect_local_function_calls(&node.block.lock(), definitions, namer, consensus)
            }
            Statement::GenericFor(node) => {
                collect_local_function_calls(&node.block.lock(), definitions, namer, consensus)
            }
            _ => {}
        }
    }
}

fn collect_calls_in_rvalue(
    value: &RValue,
    definitions: &FxHashMap<usize, Vec<RcLocal>>,
    namer: &Namer,
    consensus: &mut FxHashMap<usize, ParamConsensus>,
) {
    match value {
        RValue::Call(call) | RValue::Select(Select::Call(call)) => {
            record_local_function_call(call, definitions, namer, consensus)
        }
        RValue::Closure(closure) => {
            collect_local_function_calls(
                &closure.function.lock().body,
                definitions,
                namer,
                consensus,
            );
            return;
        }
        _ => {}
    }
    for child in value.rvalues() {
        collect_calls_in_rvalue(child, definitions, namer, consensus);
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
    name_locals_with_options(block, rename, script_name, NameLocalOptions::default());
}

pub fn name_locals_with_options(
    block: &mut Block,
    rename: bool,
    script_name: Option<&str>,
    options: NameLocalOptions,
) {
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
    let mut class_signal_locals = FxHashSet::default();
    collect_class_signals(block, &mut class_signal_locals);

    let mut namer = Namer {
        rename,
        dont_reuse_var: options.dont_reuse_var,
        module_hint: script_name.and_then(script_module_hint),
        reserved: FxHashSet::default(),
        used_file_names: FxHashSet::default(),
        next_file_suffix: FxHashMap::default(),
        hints: FxHashMap::default(),
        context_hints: FxHashMap::default(),
        isa_families: FxHashMap::default(),
        isa_conflicts: FxHashSet::default(),
        isa_derived_hints: FxHashMap::default(),
        instance_assignment_hints: FxHashMap::default(),
        instance_assignment_conflicts: FxHashSet::default(),
        module_table_locals: FxHashSet::default(),
        named: FxHashSet::default(),
        closure_locals: FxHashSet::default(),
        usage,
        create_element_aliases,
        counts,
        collapse_candidates,
        class_signal_locals,
    };
    namer.collect(block, true);
    namer.usage_based_hints();
    namer.interprocedural_param_hints(block);
    namer.apply(block);
    if rename {
        avoid_shadowing(block, FxHashMap::default());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        name_locals, name_locals_with_options, name_locals_with_script_name, pluralize,
        sanitize, sanitize_preserve,
    };
    use crate::formatter::Formatter;
    use crate::{
        Assign, Binary, BinaryOperation, Block, Call, Closure, Function, GenericFor, Global, If,
        Index, LValue, Literal, MethodCall, NumericFor, RValue, RcLocal, Return, Select, Statement,
        Table, Upvalue,
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

    #[test]
    fn identifier_hints_are_not_truncated_to_32_characters() {
        assert_eq!(
            sanitize_preserve("createAnimationFromKeyframeSequence"),
            Some("createAnimationFromKeyframeSequence".to_string())
        );
    }

    #[test]
    fn lowercase_sanitizer_preserves_constant_identifiers() {
        assert_eq!(sanitize("DEFAULT_BRUSH"), Some("DEFAULT_BRUSH".to_string()));
        assert_eq!(sanitize("RGB24"), Some("RGB24".to_string()));
        assert_eq!(sanitize("BasePart"), Some("basePart".to_string()));
    }

    #[test]
    fn constant_field_hint_preserves_its_casing() {
        let brush = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &brush,
                RValue::Index(Index::new(global("config"), string("DEFAULT_BRUSH"))),
            ),
            use_local(&brush),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&brush), "DEFAULT_BRUSH");
        assert!(
            block
                .to_string()
                .contains("local DEFAULT_BRUSH = config.DEFAULT_BRUSH")
        );
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
    fn ipairs_child_names_flow_through_dynamic_lookup() {
        let index = RcLocal::default();
        let child_name = RcLocal::default();
        let child = RcLocal::default();

        let child_decl = declare(
            &child,
            RValue::MethodCall(MethodCall::new(
                global("workspace"),
                "FindFirstChild".to_string(),
                vec![RValue::Local(child_name.clone())],
            )),
        );
        let loop_body = Block(vec![child_decl, use_local(&child)]);
        let generic_for = GenericFor::new(
            vec![index.clone(), child_name.clone()],
            vec![RValue::Call(Call::new(
                global("ipairs"),
                vec![RValue::Table(Table(vec![
                    (None, string("RevealRigs")),
                    (None, string("DoubleRigs")),
                ]))],
            ))],
            loop_body,
        );
        let mut block = Block(vec![Statement::GenericFor(generic_for)]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&index), "i");
        assert_eq!(name_of(&child_name), "childName");
        assert_eq!(name_of(&child), "child");
        let output = block.to_string();
        assert!(
            output.contains("for i, childName in ipairs"),
            "unexpected loop binder naming:\n{output}"
        );
        assert!(
            output.contains("local child = workspace:FindFirstChild(childName)"),
            "dynamic lookup result should be named child:\n{output}"
        );
    }

    #[test]
    fn dynamic_lookup_hypernym_does_not_leak_through_require() {
        let module_name = named_local("moduleName");
        let module = RcLocal::default();
        let lookup = RValue::MethodCall(MethodCall::new(
            global("script"),
            "FindFirstChild".to_string(),
            vec![RValue::Local(module_name)],
        ));
        let mut block = Block(vec![
            declare(
                &module,
                RValue::Call(Call::new(global("require"), vec![lookup])),
            ),
            use_local(&module),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&module), "v");
    }

    #[test]
    fn concrete_isa_evidence_beats_dynamic_child_hypernym() {
        let child_name = named_local("childName");
        let child = RcLocal::default();
        let lookup = RValue::MethodCall(MethodCall::new(
            global("workspace"),
            "FindFirstChild".to_string(),
            vec![RValue::Local(child_name)],
        ));
        let isa = RValue::MethodCall(MethodCall::new(
            RValue::Local(child.clone()),
            "IsA".to_string(),
            vec![string("BasePart")],
        ));
        let mut block = Block(vec![
            declare(&child, lookup),
            If::new(isa, Block(vec![use_local(&child)]), Block::default()).into(),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&child), "part");
    }

    #[test]
    fn loop_api_slot_refuses_conflicting_or_non_string_type_facts() {
        fn typeof_guard(local: &RcLocal, ty: &str) -> RValue {
            RValue::Binary(Binary::new(
                RValue::Call(Call::new(
                    global("typeof"),
                    vec![RValue::Local(local.clone())],
                )),
                string(ty),
                BinaryOperation::Equal,
            ))
        }
        fn loop_with_guards(value: &RcLocal, guards: &[&str]) -> Statement {
            let mut body = guards
                .iter()
                .map(|ty| {
                    If::new(
                        typeof_guard(value, ty),
                        Block(vec![use_local(value)]),
                        Block::default(),
                    )
                    .into()
                })
                .collect::<Vec<Statement>>();
            body.push(method_stmt(
                global("workspace"),
                "FindFirstChild",
                vec![RValue::Local(value.clone())],
            ));
            Statement::GenericFor(GenericFor::new(
                vec![RcLocal::default(), value.clone()],
                vec![RValue::Call(Call::new(
                    global("ipairs"),
                    vec![global("values")],
                ))],
                Block(body),
            ))
        }

        let number_value = RcLocal::default();
        let conflicting_value = RcLocal::default();
        let mut block = Block(vec![
            loop_with_guards(&number_value, &["number"]),
            loop_with_guards(&conflicting_value, &["string", "Instance"]),
        ]);

        name_locals(&mut block, true);

        assert_ne!(name_of(&number_value), "childName");
        assert_ne!(name_of(&conflicting_value), "childName");
    }

    #[test]
    fn conflicting_loop_api_slots_do_not_guess_a_name() {
        let index = RcLocal::default();
        let value = RcLocal::default();
        let loop_body = Block(vec![
            method_stmt(
                global("workspace"),
                "FindFirstChild",
                vec![RValue::Local(value.clone())],
            ),
            method_stmt(
                global("instance"),
                "GetAttribute",
                vec![RValue::Local(value.clone())],
            ),
        ]);
        let generic_for = GenericFor::new(
            vec![index, value.clone()],
            vec![RValue::Call(Call::new(
                global("ipairs"),
                vec![global("values")],
            ))],
            loop_body,
        );
        let mut block = Block(vec![Statement::GenericFor(generic_for)]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&value), "v");
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
    fn mixed_effect_isa_uses_family_name() {
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

        assert_eq!(name_of(&value), "effect");
    }

    #[test]
    fn later_cross_family_isa_clears_prior_family_hint() {
        let index = RcLocal::default();
        let value = RcLocal::default();
        let mut checks = Vec::new();
        for class in ["ParticleEmitter", "Beam", "Part"] {
            checks.push(
                If::new(
                    RValue::MethodCall(MethodCall::new(
                        RValue::Local(value.clone()),
                        "IsA".to_string(),
                        vec![string(class)],
                    )),
                    Block(vec![use_local(&value)]),
                    Block::default(),
                )
                .into(),
            );
        }
        let mut block = Block(vec![Statement::GenericFor(GenericFor::new(
            vec![index, value.clone()],
            vec![RValue::MethodCall(MethodCall::new(
                global("model"),
                "GetChildren".to_string(),
                vec![],
            ))],
            Block(checks),
        ))]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&value), "child");
    }

    #[test]
    fn lookup_and_constructor_class_conflict_is_refused() {
        let model = RcLocal::default();
        let lookup = RValue::MethodCall(MethodCall::new(
            global("workspace"),
            "FindFirstChild".to_string(),
            vec![string("Folder")],
        ));
        let constructor = RValue::Call(Call::new(
            RValue::Index(Index::new(global("Instance"), string("new"))),
            vec![string("Model")],
        ));
        let mut block = Block(vec![
            declare(&model, lookup),
            Statement::Assign(Assign::new(
                vec![LValue::Local(model.clone())],
                vec![constructor],
            )),
            use_local(&model),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&model), "v");
    }

    #[test]
    fn conflicting_instance_constructor_assignments_are_refused() {
        let value = RcLocal::default();
        let constructor = |class| {
            RValue::Call(Call::new(
                RValue::Index(Index::new(global("Instance"), string("new"))),
                vec![string(class)],
            ))
        };
        let mut block = Block(vec![
            declare(&value, RValue::Literal(Literal::Nil)),
            Statement::Assign(Assign::new(
                vec![LValue::Local(value.clone())],
                vec![constructor("Model")],
            )),
            Statement::Assign(Assign::new(
                vec![LValue::Local(value.clone())],
                vec![constructor("Folder")],
            )),
            use_local(&value),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&value), "v");
    }

    #[test]
    fn non_instance_write_invalidates_constructor_consensus() {
        let value = RcLocal::default();
        let constructor = RValue::Call(Call::new(
            RValue::Index(Index::new(global("Instance"), string("new"))),
            vec![string("Model")],
        ));
        let mut block = Block(vec![
            declare(&value, constructor),
            Statement::Assign(Assign::new(
                vec![LValue::Local(value.clone())],
                vec![string("done")],
            )),
            use_local(&value),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&value), "v");
    }

    #[test]
    fn multi_result_write_invalidates_constructor_consensus() {
        let ignored = RcLocal::default();
        let value = RcLocal::default();
        let constructor = RValue::Call(Call::new(
            RValue::Index(Index::new(global("Instance"), string("new"))),
            vec![string("Model")],
        ));
        let mut block = Block(vec![
            declare(&value, constructor),
            Statement::Assign(Assign::new(
                vec![LValue::Local(ignored), LValue::Local(value.clone())],
                vec![RValue::Select(Select::Call(Call::new(
                    global("getState"),
                    vec![],
                )))],
            )),
            use_local(&value),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&value), "v");
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

    #[test]
    fn dont_reuse_var_suffixes_regular_sibling_closure_locals() {
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

        let mut block = Block(vec![
            Statement::Call(Call::new(closure_a, vec![])),
            Statement::Call(Call::new(closure_b, vec![])),
        ]);

        name_locals_with_options(
            &mut block,
            true,
            None,
            super::NameLocalOptions {
                dont_reuse_var: true,
            },
        );

        assert_eq!(name_of(&part_a), "part");
        assert_eq!(name_of(&part_b), "part2");
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
    fn dont_reuse_var_keeps_sibling_loop_headers_reusable() {
        let i_a = RcLocal::default();
        let i_b = RcLocal::default();
        let k_a = RcLocal::default();
        let v_a = RcLocal::default();
        let k_b = RcLocal::default();
        let v_b = RcLocal::default();

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
        let generic_a = Statement::GenericFor(GenericFor::new(
            vec![k_a.clone(), v_a.clone()],
            vec![global("pairs")],
            Block(vec![use_local(&k_a), use_local(&v_a)]),
        ));
        let generic_b = Statement::GenericFor(GenericFor::new(
            vec![k_b.clone(), v_b.clone()],
            vec![global("pairs")],
            Block(vec![use_local(&k_b), use_local(&v_b)]),
        ));

        let mut block = Block(vec![for_a, for_b, generic_a, generic_b]);
        name_locals_with_options(
            &mut block,
            true,
            None,
            super::NameLocalOptions {
                dont_reuse_var: true,
            },
        );

        assert_eq!(name_of(&i_a), "i");
        assert_eq!(name_of(&i_b), "i");
        assert_eq!(name_of(&k_a), "k");
        assert_eq!(name_of(&v_a), "v");
        assert_eq!(name_of(&k_b), "k");
        assert_eq!(name_of(&v_b), "v");
    }

    #[test]
    fn dont_reuse_var_regular_local_avoids_prior_loop_header_name() {
        let k = RcLocal::default();
        let loop_v = RcLocal::default();
        let regular_v = RcLocal::default();

        let generic = Statement::GenericFor(GenericFor::new(
            vec![k.clone(), loop_v.clone()],
            vec![global("pairs")],
            Block(vec![use_local(&loop_v)]),
        ));
        let mut block = Block(vec![
            generic,
            declare(&regular_v, number(1.0)),
            use_local(&regular_v),
        ]);

        name_locals_with_options(
            &mut block,
            true,
            None,
            super::NameLocalOptions {
                dont_reuse_var: true,
            },
        );

        assert_eq!(name_of(&loop_v), "v");
        assert_eq!(name_of(&regular_v), "v2");
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

    /// A parameter that is invoked (`p()`) is a callback, not a record: `props`
    /// is refused (used_as_callee), and the `callee_callback` dataflow signal
    /// then names it `callback`.
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
        assert_ne!(name_of(&p), "props");
        assert_eq!(name_of(&p), "callback");
    }

    #[test]
    fn props_param_refused_when_invoked_as_selected_call() {
        let p = RcLocal::default();
        let (a, b, c) = (RcLocal::default(), RcLocal::default(), RcLocal::default());
        let out_a = RcLocal::default();
        let out_b = RcLocal::default();
        let mut selected = Assign::new(
            vec![LValue::Local(out_a), LValue::Local(out_b)],
            vec![RValue::Select(Select::Call(Call::new(
                RValue::Local(p.clone()),
                vec![],
            )))],
        );
        selected.prefix = true;
        let mut function = Function::default();
        function.parameters = vec![p.clone()];
        function.body = Block(vec![
            declare(&a, field(&p, "x")),
            declare(&b, field(&p, "y")),
            declare(&c, field(&p, "z")),
            Statement::Assign(selected),
            ret(vec![create_element(vec![string("Frame")])]),
        ]);
        let comp = RcLocal::default();
        let mut block = Block(vec![declare(&comp, closure_of(function)), use_local(&comp)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&p), "callback");
    }

    // ===== §param dataflow naming v1 =====

    /// Build a top-level closure with the given params + body, name the whole
    /// program, then the params' names can be read with `name_of`.
    fn name_param_fn(params: Vec<RcLocal>, body: Vec<Statement>) {
        let mut function = Function::default();
        function.parameters = params;
        function.body = Block(body);
        let f = RcLocal::default();
        let mut block = Block(vec![declare(&f, closure_of(function)), use_local(&f)]);
        name_locals(&mut block, true);
    }

    fn method_stmt(receiver: RValue, method: &str, args: Vec<RValue>) -> Statement {
        Statement::MethodCall(MethodCall::new(receiver, method.to_string(), args))
    }

    /// `self.Range = p` — a param stored into a named field takes the field name.
    #[test]
    fn field_store_names_param_from_key() {
        let (this, p) = (RcLocal::default(), RcLocal::default());
        name_param_fn(
            vec![this.clone(), p.clone()],
            vec![keyed_assign(
                &this,
                string("Range"),
                RValue::Local(p.clone()),
            )],
        );
        assert_eq!(name_of(&p), "range");
    }

    /// A private-field convention key (`self._balance = p`) strips the leading `_`.
    #[test]
    fn field_store_strips_leading_underscore() {
        let (this, p) = (RcLocal::default(), RcLocal::default());
        name_param_fn(
            vec![this.clone(), p.clone()],
            vec![keyed_assign(
                &this,
                string("_balance"),
                RValue::Local(p.clone()),
            )],
        );
        assert_eq!(name_of(&p), "balance");
    }

    /// A generic-content destination key (`self.Value = p`) is no better than `p`,
    /// so it is refused.
    #[test]
    fn field_store_generic_key_refused() {
        let (this, p) = (RcLocal::default(), RcLocal::default());
        name_param_fn(
            vec![this.clone(), p.clone()],
            vec![keyed_assign(
                &this,
                string("Value"),
                RValue::Local(p.clone()),
            )],
        );
        assert_ne!(name_of(&p), "value");
        assert_eq!(name_of(&p), "p2"); // param0 `this` takes `p`; refused param1 -> `p2`
    }

    /// A param stored into two *different* fields is ambiguous — refuse.
    #[test]
    fn field_store_conflict_refused() {
        let (this, p) = (RcLocal::default(), RcLocal::default());
        name_param_fn(
            vec![this.clone(), p.clone()],
            vec![
                keyed_assign(&this, string("Range"), RValue::Local(p.clone())),
                keyed_assign(&this, string("Width"), RValue::Local(p.clone())),
            ],
        );
        assert_eq!(name_of(&p), "p2"); // param0 `this` takes `p`; refused param1 -> `p2`
    }

    /// A param WRAPPED in a constructor on the RHS (`self.CFrame = CFrame.new(p)`)
    /// is NOT the field's value, so it is not named from the field.
    #[test]
    fn field_store_wrapped_rhs_skipped() {
        let (this, p) = (RcLocal::default(), RcLocal::default());
        let wrapped = RValue::Call(Call::new(
            RValue::Index(Index::new(global("CFrame"), string("new"))),
            vec![RValue::Local(p.clone())],
        ));
        name_param_fn(
            vec![this.clone(), p.clone()],
            vec![keyed_assign(&this, string("CFrame"), wrapped)],
        );
        assert_ne!(name_of(&p), "cFrame");
        assert_eq!(name_of(&p), "p2"); // param0 `this` takes `p`; refused param1 -> `p2`
    }

    /// `workspace:FindFirstChild(p)` — the lookup argument is a child-name string.
    #[test]
    fn find_first_child_arg_named_child_name() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![method_stmt(
                global("workspace"),
                "FindFirstChild",
                vec![RValue::Local(p.clone())],
            )],
        );
        assert_eq!(name_of(&p), "childName");
    }

    /// `x:SetAttribute("PlantKey", p)` — the literal key names the value.
    #[test]
    fn set_attribute_literal_key_names_value() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![method_stmt(
                global("model"),
                "SetAttribute",
                vec![string("PlantKey"), RValue::Local(p.clone())],
            )],
        );
        assert_eq!(name_of(&p), "plantKey");
    }

    /// A child-name arg that is ALSO used as an Instance receiver is contradicted
    /// — the name-string hint is refused and the instance shape wins.
    #[test]
    fn child_name_refused_when_also_instance() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![
                method_stmt(
                    global("workspace"),
                    "WaitForChild",
                    vec![RValue::Local(p.clone())],
                ),
                method_stmt(RValue::Local(p.clone()), "Destroy", vec![]),
            ],
        );
        assert_ne!(name_of(&p), "childName");
        assert_eq!(name_of(&p), "instance");
    }

    /// A param used as a string method receiver (`p:gsub(...)`) is a string value.
    #[test]
    fn string_method_receiver_named_value() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![method_stmt(
                RValue::Local(p.clone()),
                "gsub",
                vec![string("%s"), string("")],
            )],
        );
        assert_eq!(name_of(&p), "value");
    }

    /// `local x = p or {}` — an empty-table default reveals an options table.
    #[test]
    fn or_default_empty_table_named_options() {
        let (p, x) = (RcLocal::default(), RcLocal::default());
        let or_default = RValue::Binary(Binary::new(
            RValue::Local(p.clone()),
            RValue::Table(Table::default()),
            BinaryOperation::Or,
        ));
        name_param_fn(
            vec![p.clone()],
            vec![declare(&x, or_default), use_local(&x)],
        );
        assert_eq!(name_of(&p), "options");
    }

    /// An invoked param (`p()`) with no stronger evidence reads as a callback.
    #[test]
    fn invoked_param_named_callback() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![Statement::Call(Call::new(RValue::Local(p.clone()), vec![]))],
        );
        assert_eq!(name_of(&p), "callback");
    }

    /// A field-store dataflow fact (48) outranks a weaker type hypernym (string
    /// method => value, 40) on the same param.
    #[test]
    fn field_store_beats_string_method() {
        let (this, p) = (RcLocal::default(), RcLocal::default());
        name_param_fn(
            vec![this.clone(), p.clone()],
            vec![
                keyed_assign(&this, string("Label"), RValue::Local(p.clone())),
                method_stmt(
                    RValue::Local(p.clone()),
                    "gsub",
                    vec![string("a"), string("b")],
                ),
            ],
        );
        assert_eq!(name_of(&p), "label");
    }

    /// A digit-suffixed field key (`self.BackgroundColor3 = p`) strips the
    /// trailing type digit so the disambiguator can't chain a `backgroundColor33`.
    #[test]
    fn field_store_strips_trailing_digit() {
        let (this, p) = (RcLocal::default(), RcLocal::default());
        name_param_fn(
            vec![this.clone(), p.clone()],
            vec![keyed_assign(
                &this,
                string("BackgroundColor3"),
                RValue::Local(p.clone()),
            )],
        );
        assert_eq!(name_of(&p), "backgroundColor");
    }

    /// `obj:GetAttribute(p)` — the single argument is an attribute-name string.
    #[test]
    fn get_attribute_arg_named_attribute_name() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![method_stmt(
                global("model"),
                "GetAttribute",
                vec![RValue::Local(p.clone())],
            )],
        );
        assert_eq!(name_of(&p), "attributeName");
    }

    /// A leading-`_` attribute key strips the underscore (shared with field-store
    /// sanitization): `SetAttribute("_internal", p)` -> `internal`.
    #[test]
    fn set_attribute_strips_leading_underscore() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![method_stmt(
                global("model"),
                "SetAttribute",
                vec![string("_internalFlag"), RValue::Local(p.clone())],
            )],
        );
        assert_eq!(name_of(&p), "internalFlag");
    }

    /// A param that sets two *different* attribute keys is ambiguous — refuse.
    #[test]
    fn attr_key_conflict_refused() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![
                method_stmt(
                    global("a"),
                    "SetAttribute",
                    vec![string("Foo"), RValue::Local(p.clone())],
                ),
                method_stmt(
                    global("b"),
                    "SetAttribute",
                    vec![string("Bar"), RValue::Local(p.clone())],
                ),
            ],
        );
        assert_eq!(name_of(&p), "p");
    }

    /// A param that fills two *different* API slots (child name vs attribute name)
    /// is ambiguous — refuse.
    #[test]
    fn api_slot_conflict_refused() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![
                method_stmt(
                    global("a"),
                    "FindFirstChild",
                    vec![RValue::Local(p.clone())],
                ),
                method_stmt(global("b"), "GetAttribute", vec![RValue::Local(p.clone())]),
            ],
        );
        assert_eq!(name_of(&p), "p");
    }

    /// `local x = p or 1` — a numeric default reveals a scalar value.
    #[test]
    fn or_default_number_named_value() {
        let (p, x) = (RcLocal::default(), RcLocal::default());
        let or_default = RValue::Binary(Binary::new(
            RValue::Local(p.clone()),
            number(1.0),
            BinaryOperation::Or,
        ));
        name_param_fn(
            vec![p.clone()],
            vec![declare(&x, or_default), use_local(&x)],
        );
        assert_eq!(name_of(&p), "value");
    }

    /// An invoked param contradicted by a `typeof(p) == "string"` guard is NOT a
    /// callback — the string type wins (`value`).
    #[test]
    fn callee_refused_when_typeof_string() {
        let p = RcLocal::default();
        let guard = RValue::Binary(Binary::new(
            RValue::Call(Call::new(global("typeof"), vec![RValue::Local(p.clone())])),
            string("string"),
            BinaryOperation::Equal,
        ));
        name_param_fn(
            vec![p.clone()],
            vec![
                Statement::Call(Call::new(RValue::Local(p.clone()), vec![])),
                declare(&RcLocal::default(), guard),
            ],
        );
        assert_ne!(name_of(&p), "callback");
        assert_eq!(name_of(&p), "value");
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

    #[test]
    fn connection_collection_named_from_table_insert() {
        let connections = RcLocal::default();
        let callback = RcLocal::default();
        let connection = RValue::MethodCall(MethodCall::new(
            global("signal"),
            "Connect".to_string(),
            vec![RValue::Local(callback)],
        ));
        let insert = Statement::Call(Call::new(
            RValue::Index(Index::new(global("table"), string("insert"))),
            vec![RValue::Local(connections.clone()), connection],
        ));
        let mut block = Block(vec![
            declare(&connections, RValue::Table(Table::default())),
            insert,
            use_local(&connections),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&connections), "connections");
    }

    #[test]
    fn mixed_collection_fill_refuses_connections_name() {
        let values = RcLocal::default();
        let callback = RcLocal::default();
        let connection = RValue::MethodCall(MethodCall::new(
            global("signal"),
            "Connect".to_string(),
            vec![RValue::Local(callback)],
        ));
        let insert = |value| {
            Statement::Call(Call::new(
                RValue::Index(Index::new(global("table"), string("insert"))),
                vec![RValue::Local(values.clone()), value],
            ))
        };
        let mut block = Block(vec![
            declare(&values, RValue::Table(Table::default())),
            insert(connection),
            insert(string("not a connection")),
            use_local(&values),
        ]);

        name_locals(&mut block, true);

        assert_ne!(name_of(&values), "connections");
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

    #[test]
    fn forward_declared_callback_named_from_event_use() {
        let handler = RcLocal::default();
        let declaration = Statement::Assign(Assign {
            left: vec![LValue::Local(handler.clone())],
            right: Vec::new(),
            prefix: true,
            parallel: false,
        });
        let definition = Statement::Assign(Assign::new(
            vec![LValue::Local(handler.clone())],
            vec![closure_of(Function::default())],
        ));
        let connect = Statement::MethodCall(MethodCall::new(
            RValue::Index(Index::new(global("folder"), string("ChildAdded"))),
            "Connect".to_string(),
            vec![RValue::Local(handler.clone())],
        ));
        let mut block = Block(vec![declaration, definition, connect]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&handler), "onChildAdded");
    }

    #[test]
    fn debug_function_name_outranks_event_context() {
        let handler = RcLocal::default();
        let mut function = Function::default();
        function.name = Some("rebuildCache".to_string());
        let mut block = Block(vec![
            declare(&handler, closure_of(function)),
            Statement::MethodCall(MethodCall::new(
                RValue::Index(Index::new(global("folder"), string("ChildAdded"))),
                "Connect".to_string(),
                vec![RValue::Local(handler.clone())],
            )),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&handler), "rebuildCache");
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
        let table = Table(vec![(
            Some(string("setVisible")),
            RValue::Local(setter.clone()),
        )]);
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

    /// Problem 1: `local character = localPlayer.Character or
    /// localPlayer.CharacterAdded:Wait()`. The `or`'s LEFT (primary) operand is a
    /// field read, so the local is named after that field; the method-call
    /// fallback on the right is not consulted.
    #[test]
    fn or_primary_field_names_local() {
        let local_player = named_local("localPlayer");
        let character = RcLocal::default();
        let value = RValue::Binary(Binary::new(
            RValue::Index(Index::new(
                RValue::Local(local_player.clone()),
                string("Character"),
            )),
            RValue::MethodCall(MethodCall::new(
                RValue::Index(Index::new(
                    RValue::Local(local_player.clone()),
                    string("CharacterAdded"),
                )),
                "Wait".to_string(),
                vec![],
            )),
            BinaryOperation::Or,
        ));
        let mut block = Block(vec![
            declare(&character, value),
            use_local(&character),
            use_local(&local_player),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&character), "character");
    }

    /// An `and`-guard whose RIGHT (guarded) operand is a plain field read is now
    /// named after that field: `local parent = inst and inst.Parent` -> `parent`.
    #[test]
    fn and_guard_field_rhs_names_local() {
        let inst = named_local("inst");
        let parent = RcLocal::default();
        let value = RValue::Binary(Binary::new(
            RValue::Local(inst.clone()),
            RValue::Index(Index::new(RValue::Local(inst.clone()), string("Parent"))),
            BinaryOperation::And,
        ));
        let mut block = Block(vec![
            declare(&parent, value),
            use_local(&parent),
            use_local(&inst),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&parent), "parent");
    }

    /// Regression anchor: the nil-guarded *method-call* lookup is unchanged by the
    /// generalized binary hint — `inst and inst:FindFirstChild("Humanoid")` still
    /// names `humanoid` (And -> right MethodCall -> method_call_hint).
    #[test]
    fn and_guard_method_lookup_still_named() {
        let inst = named_local("inst");
        let humanoid = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &humanoid,
                guarded_find(RValue::Local(inst.clone()), "Humanoid"),
            ),
            use_local(&humanoid),
            use_local(&inst),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&humanoid), "humanoid");
    }

    /// A left-associated `or` chain names after the LEFTMOST primary:
    /// `local first = a.First or b or c` -> `first` (Or -> left -> Or -> left ->
    /// Index). Exercises the recursive descent through nested `Or`.
    #[test]
    fn or_left_associated_chain_names_leftmost() {
        let first = RcLocal::default();
        let inner = RValue::Binary(Binary::new(
            RValue::Index(Index::new(global("a"), string("First"))),
            global("b"),
            BinaryOperation::Or,
        ));
        let value = RValue::Binary(Binary::new(inner, global("c"), BinaryOperation::Or));
        let mut block = Block(vec![declare(&first, value), use_local(&first)]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&first), "first");
    }

    /// A binary whose chosen operand carries no name (`alpha and beta`, both bare
    /// locals) leaves the local at its default generated name — the soundness
    /// boundary: we never invent a name from an unnameable operand.
    #[test]
    fn binary_with_unnameable_operands_stays_default() {
        let alpha = named_local("alpha");
        let beta = named_local("beta");
        let result = RcLocal::default();
        let value = RValue::Binary(Binary::new(
            RValue::Local(alpha.clone()),
            RValue::Local(beta.clone()),
            BinaryOperation::And,
        ));
        let mut block = Block(vec![
            declare(&result, value),
            use_local(&result),
            use_local(&alpha),
            use_local(&beta),
        ]);

        name_locals(&mut block, true);
        assert_eq!(name_of(&result), "v");
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
            declare(
                &seeds,
                guarded_find(RValue::Local(world.clone()), "PlantedSeeds"),
            ),
            use_local(&seeds),
            declare(
                &pots,
                guarded_find(RValue::Local(world.clone()), "PlacedPots"),
            ),
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
            declare(
                &seeds,
                guarded_find(RValue::Local(world.clone()), "PlantedSeeds"),
            ),
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

    /// A dynamic (non-literal) lookup cannot recover a concrete child name, but
    /// the API contract still proves that the yielded value is a child.
    #[test]
    fn guarded_dynamic_lookup_uses_child_hypernym() {
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
        assert_eq!(name_of(&result), "child");
    }

    #[test]
    fn dynamic_lookup_refuses_non_child_or_fallback() {
        let child_name = named_local("childName");
        let result = RcLocal::default();
        let lookup = RValue::MethodCall(MethodCall::new(
            global("workspace"),
            "FindFirstChild".to_string(),
            vec![RValue::Local(child_name)],
        ));
        let with_table_fallback =
            RValue::Binary(Binary::new(lookup, RValue::Table(Table::default()), BinaryOperation::Or));
        let mut block = Block(vec![declare(&result, with_table_fallback), use_local(&result)]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&result), "v");
    }

    #[test]
    fn dynamic_lookup_allows_nil_or_fallback() {
        let child_name = named_local("childName");
        let result = RcLocal::default();
        let lookup = RValue::MethodCall(MethodCall::new(
            global("workspace"),
            "FindFirstChild".to_string(),
            vec![RValue::Local(child_name)],
        ));
        let with_nil_fallback = RValue::Binary(Binary::new(
            lookup,
            RValue::Literal(Literal::Nil),
            BinaryOperation::Or,
        ));
        let mut block = Block(vec![declare(&result, with_nil_fallback), use_local(&result)]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&result), "child");
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
            declare(
                &seeds,
                guarded_find(RValue::Local(world.clone()), "PlantedSeeds"),
            ),
            use_local(&seeds),
            declare(
                &pots,
                guarded_find(RValue::Local(world.clone()), "PlacedPots"),
            ),
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
            If::new(
                guard("string"),
                Block(vec![use_local(&p)]),
                Block::default(),
            )
            .into(),
            If::new(
                guard("number"),
                Block(vec![use_local(&p)]),
                Block::default(),
            )
            .into(),
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

    /// `.UserId` alone can describe a plain data record, so it must not invent
    /// the semantically stronger `player` name.
    #[test]
    fn user_id_field_alone_does_not_name_player() {
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
        assert_eq!(name_of(&p), "p");
    }

    /// `.Character` is a Player-specific field and remains a strong signal.
    #[test]
    fn character_field_names_param_player() {
        let p = RcLocal::default();
        name_param_fn(
            vec![p.clone()],
            vec![Statement::Call(Call::new(
                global("print"),
                vec![field(&p, "Character")],
            ))],
        );
        assert_eq!(name_of(&p), "player");
    }

    #[test]
    fn standard_library_slots_name_parameters() {
        let value = RcLocal::default();
        let min = RcLocal::default();
        let max = RcLocal::default();
        let duration = RcLocal::default();
        let clamp = RValue::Index(Index::new(global("math"), string("clamp")));
        let wait = RValue::Index(Index::new(global("task"), string("wait")));
        name_param_fn(
            vec![value.clone(), min.clone(), max.clone(), duration.clone()],
            vec![
                Statement::Call(Call::new(
                    clamp,
                    vec![
                        RValue::Local(value.clone()),
                        RValue::Local(min.clone()),
                        RValue::Local(max.clone()),
                    ],
                )),
                Statement::Call(Call::new(wait, vec![RValue::Local(duration.clone())])),
            ],
        );
        assert_eq!(name_of(&value), "value");
        assert_eq!(name_of(&min), "min");
        assert_eq!(name_of(&max), "max");
        assert_eq!(name_of(&duration), "duration");
    }

    #[test]
    fn unanimous_call_sites_name_local_function_parameter() {
        let parameter = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![parameter.clone()];
        function.body = Block(vec![use_local(&parameter)]);
        let binder = RcLocal::default();
        let call = |owner: &str, key: &str| {
            Statement::Call(Call::new(
                RValue::Local(binder.clone()),
                vec![RValue::Index(Index::new(global(owner), string(key)))],
            ))
        };
        let mut block = Block(vec![
            declare(&binder, closure_of(function)),
            call("record", "PetData"),
            call("cache", "PetData"),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&parameter), "petData");
    }

    #[test]
    fn disagreeing_call_sites_do_not_name_parameter() {
        let parameter = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![parameter.clone()];
        function.body = Block(vec![use_local(&parameter)]);
        let binder = RcLocal::default();
        let mut block = Block(vec![
            declare(&binder, closure_of(function)),
            Statement::Call(Call::new(
                RValue::Local(binder.clone()),
                vec![RValue::Index(Index::new(
                    global("record"),
                    string("PetData"),
                ))],
            )),
            Statement::Call(Call::new(
                RValue::Local(binder),
                vec![RValue::Index(Index::new(
                    global("record"),
                    string("Config"),
                ))],
            )),
        ]);

        name_locals(&mut block, true);

        assert_eq!(name_of(&parameter), "p");
    }

    #[test]
    fn reassigned_function_binder_disables_callsite_consensus() {
        let parameter = RcLocal::default();
        let mut function = Function::default();
        function.parameters = vec![parameter.clone()];
        function.body = Block(vec![use_local(&parameter)]);
        let binder = RcLocal::default();
        let mut block = Block(vec![
            declare(&binder, closure_of(function)),
            Statement::Assign(Assign::new(
                vec![LValue::Local(binder.clone())],
                vec![global("other")],
            )),
            Statement::Call(Call::new(
                RValue::Local(binder),
                vec![RValue::Index(Index::new(
                    global("record"),
                    string("PetData"),
                ))],
            )),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&parameter), "p");
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

    /// A non-predicate call whose callee is also not a factory/getter verb
    /// (`frobnicate`) is left alone by both Layer A (predicate) and the verb-call
    /// hint -> default name.
    #[test]
    fn predicate_non_predicate_call_refused() {
        let v = RcLocal::default();
        let mut block = Block(vec![
            declare(&v, predicate_call("frobnicate")),
            use_local(&v),
        ]);
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
                RValue::Call(Call::new(
                    RValue::Local(is_ready.clone()),
                    vec![global("x")],
                )),
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
        let cmp = bool_compare(
            index_of("obj", "Visible"),
            boolean(true),
            BinaryOperation::Equal,
        );
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "visible");
    }

    // ---- Bucket A: factory/getter verb-strip call naming ----

    /// `local v = getOwnPlot()` -> `ownPlot`.
    #[test]
    fn verb_call_get_names_subject() {
        let v = RcLocal::default();
        let call = RValue::Call(Call::new(global("getOwnPlot"), vec![]));
        let mut block = Block(vec![declare(&v, call), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "ownPlot");
    }

    /// `createButton(...)` -> `button`; `normalizeContentId(...)` -> `contentId`.
    #[test]
    fn verb_call_create_and_normalize() {
        let b = RcLocal::default();
        let c = RcLocal::default();
        let mut block = Block(vec![
            declare(&b, RValue::Call(Call::new(global("createButton"), vec![]))),
            declare(
                &c,
                RValue::Call(Call::new(global("normalizeContentId"), vec![global("x")])),
            ),
            use_local(&b),
            use_local(&c),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&b), "button");
        assert_eq!(name_of(&c), "contentId");
    }

    /// Compound factory verb: `getOrCreateWorkspaceFolder(...)` -> `workspaceFolder`,
    /// never the garbage `orCreateWorkspaceFolder` a naive `get`-only strip yields.
    /// (The `FX` acronym case `getOrCreateFXPart` -> `fXPart` is the standard
    /// lowerCamel sanitize artifact, exercised by the assertion below.)
    #[test]
    fn verb_call_compound_getorcreate() {
        let folder = RcLocal::default();
        let fx = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &folder,
                RValue::Call(Call::new(global("getOrCreateWorkspaceFolder"), vec![])),
            ),
            declare(
                &fx,
                RValue::Call(Call::new(global("getOrCreateFXPart"), vec![])),
            ),
            use_local(&folder),
            use_local(&fx),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&folder), "workspaceFolder");
        assert_eq!(name_of(&fx), "fXPart");
    }

    /// `summarize*` is excluded (deny-list): `summarizeStatus(...)` would name
    /// the result `status`, inverting a summary-of-status into a status. Stays `v`.
    #[test]
    fn verb_call_summarize_refused() {
        let v = RcLocal::default();
        let call = RValue::Call(Call::new(global("summarizeStatus"), vec![global("x")]));
        let mut block = Block(vec![declare(&v, call), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    // ---- Bucket B: TweenService:Create -> tween (receiver-gated) ----

    /// `local tween = TweenService:Create(...)` where TweenService is the
    /// GetService-preserved header local.
    #[test]
    fn tween_create_named_when_receiver_is_tween_service() {
        let svc = RcLocal::default();
        let v = RcLocal::default();
        let svc_value = RValue::MethodCall(MethodCall::new(
            global("game"),
            "GetService".to_string(),
            vec![string("TweenService")],
        ));
        let create = RValue::MethodCall(MethodCall::new(
            RValue::Local(svc.clone()),
            "Create".to_string(),
            vec![global("inst"), global("info")],
        ));
        let mut block = Block(vec![
            declare(&svc, svc_value),
            declare(&v, create),
            use_local(&v),
            use_local(&svc),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&svc), "TweenService");
        assert_eq!(name_of(&v), "tween");
    }

    /// A custom class constructor `SomeClass:Create()` is NOT a tween — receiver
    /// gate refuses, so the result keeps the default name.
    #[test]
    fn create_on_non_tween_service_refused() {
        let v = RcLocal::default();
        let create = RValue::MethodCall(MethodCall::new(
            global("EmiliaFBXTalkFX"),
            "Create".to_string(),
            vec![],
        ));
        let mut block = Block(vec![declare(&v, create), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    // ---- Bucket C: :Raycast -> raycastResult ----

    /// `local v = workspace:Raycast(...)` -> `raycastResult` (type-accurate for any
    /// receiver).
    #[test]
    fn raycast_names_raycast_result() {
        let v = RcLocal::default();
        let raycast = RValue::MethodCall(MethodCall::new(
            global("workspace"),
            "Raycast".to_string(),
            vec![global("origin"), global("dir")],
        ));
        let mut block = Block(vec![declare(&v, raycast), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "raycastResult");
    }

    // ---- OOP class-table naming ----

    /// An empty table with a metatable signal (`t.__index = t`) is named `class`.
    #[test]
    fn empty_table_with_index_signal_named_class() {
        let t = RcLocal::default();
        let index_assign: Statement = Assign::new(
            vec![LValue::Index(Index::new(
                RValue::Local(t.clone()),
                string("__index"),
            ))],
            vec![RValue::Local(t.clone())],
        )
        .into();
        let mut block = Block(vec![
            declare(&t, RValue::Table(Table::default())),
            index_assign,
            use_local(&t),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&t), "class");
    }

    /// An empty table that is colon-invoked (`t:method()`) is a class even without
    /// `__index` (matches the `collision.client.luau` shape).
    #[test]
    fn empty_table_colon_called_named_class() {
        let t = RcLocal::default();
        let colon: Statement = Statement::MethodCall(MethodCall::new(
            RValue::Local(t.clone()),
            "DoThing".to_string(),
            vec![],
        ));
        let mut block = Block(vec![
            declare(&t, RValue::Table(Table::default())),
            colon,
            use_local(&t),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&t), "class");
    }

    /// A plain empty table with no class signal must NOT be named `class`.
    #[test]
    fn empty_table_without_signal_not_class() {
        let t = RcLocal::default();
        let field_assign: Statement = Assign::new(
            vec![LValue::Index(Index::new(
                RValue::Local(t.clone()),
                string("x"),
            ))],
            vec![number(1.0)],
        )
        .into();
        let mut block = Block(vec![
            declare(&t, RValue::Table(Table::default())),
            field_assign,
            use_local(&t),
        ]);
        name_locals(&mut block, true);
        assert_ne!(name_of(&t), "class");
    }

    /// A SINGLE-USE empty-table colon-call temp must NOT be named `class`: an empty
    /// `{}` is movable, so `inline_single_use_temps` would fold it; naming would
    /// suppress that inline (+lines). The reads==1/writes==1 guard refuses it even
    /// though the colon-call still flags it as a class signal.
    #[test]
    fn single_use_empty_table_colon_call_not_class() {
        let t = RcLocal::default();
        let colon: Statement = Statement::MethodCall(MethodCall::new(
            RValue::Local(t.clone()),
            "DoThing".to_string(),
            vec![],
        ));
        let mut block = Block(vec![declare(&t, RValue::Table(Table::default())), colon]);
        name_locals(&mut block, true);
        assert_ne!(name_of(&t), "class");
    }

    /// A verb-strip whose remainder leads with a preposition/conjunction is refused
    /// (reads as a qualifier, worse than `vN`): `cloneFromNode` -> `FromNode` and
    /// `cloneAndPosition` -> `AndPosition` both stay `v`.
    #[test]
    fn verb_call_connective_remainder_refused() {
        let a = RcLocal::default();
        let b = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &a,
                RValue::Call(Call::new(global("cloneFromNode"), vec![global("x")])),
            ),
            declare(
                &b,
                RValue::Call(Call::new(global("cloneAndPosition"), vec![global("x")])),
            ),
            use_local(&a),
            use_local(&b),
        ]);
        name_locals(&mut block, true);
        // Refused -> the misleading connective name is never emitted; both fall back
        // to the generic `vN` form.
        assert_ne!(name_of(&a), "fromNode");
        assert_ne!(name_of(&b), "andPosition");
        assert!(name_of(&a).starts_with('v'), "got {}", name_of(&a));
        assert!(name_of(&b).starts_with('v'), "got {}", name_of(&b));
    }

    /// A genuine noun that merely starts with a connective's letters is NOT refused
    /// (`getInventory` -> `Inventory` != `In`, `getOutput` -> `Output` != `Out`).
    #[test]
    fn verb_call_noun_resembling_connective_kept() {
        let inv = RcLocal::default();
        let out = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &inv,
                RValue::Call(Call::new(global("getInventory"), vec![])),
            ),
            declare(&out, RValue::Call(Call::new(global("getOutput"), vec![]))),
            use_local(&inv),
            use_local(&out),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&inv), "inventory");
        assert_eq!(name_of(&out), "output");
    }

    /// A compound verb with no trailing noun (`getOrCreate`) is refused (would
    /// otherwise name the local after the bare factory verb `create`).
    #[test]
    fn verb_call_compound_without_noun_refused() {
        let v = RcLocal::default();
        let call = RValue::Call(Call::new(global("getOrCreate"), vec![]));
        let mut block = Block(vec![declare(&v, call), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// `X.Locked == false` is negated polarity (the value is true when Locked is
    /// FALSE), so naming it `locked` would mislead -> refused.
    #[test]
    fn bool_field_eq_false_refused() {
        let v = RcLocal::default();
        let cmp = bool_compare(
            index_of("obj", "Locked"),
            boolean(false),
            BinaryOperation::Equal,
        );
        let mut block = Block(vec![declare(&v, cmp), use_local(&v)]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&v), "v");
    }

    /// `X.Enabled ~= false` is positive polarity (true when Enabled is truthy — the
    /// default-true idiom), matching source (`UseFXTop ~= false` -> `useFXTop`).
    #[test]
    fn bool_field_neq_false_names_field() {
        let v = RcLocal::default();
        let cmp = bool_compare(
            index_of("obj", "Enabled"),
            boolean(false),
            BinaryOperation::NotEqual,
        );
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
        let cmp = bool_compare(
            index_of("obj", "Favorite"),
            boolean(true),
            BinaryOperation::NotEqual,
        );
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
        let cmp = bool_compare(
            index_of("obj", "Count"),
            number(5.0),
            BinaryOperation::Equal,
        );
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
        let cmp = bool_compare(
            index_of("obj", "_isOpen"),
            boolean(true),
            BinaryOperation::Equal,
        );
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
        let cmp = bool_compare(
            boolean(true),
            index_of("obj", "Visible"),
            BinaryOperation::Equal,
        );
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
            declare(
                &track,
                method_call(global("humanoid"), "LoadAnimation", vec![]),
            ),
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
                call(
                    global("tonumber"),
                    vec![RValue::Index(Index::new(
                        global("config"),
                        string("PlaceId"),
                    ))],
                ),
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
    fn names_bare_time_as_now() {
        let a = RcLocal::default();
        let b = RcLocal::default();
        let mut block = Block(vec![
            declare(
                &a,
                call(
                    RValue::Index(Index::new(global("os"), string("clock"))),
                    vec![],
                ),
            ),
            use_local(&a),
            declare(&b, call(global("tick"), vec![])),
            use_local(&b),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&a), "now");
        assert_eq!(name_of(&b), "now2");
    }

    #[test]
    fn saved_clock_subtraction_base_named_last_time() {
        let saved = RcLocal::default();
        let clock = || {
            call(
                RValue::Index(Index::new(global("os"), string("clock"))),
                vec![],
            )
        };
        let mut block = Block(vec![
            declare(&saved, clock()),
            Statement::Call(Call::new(
                global("print"),
                vec![RValue::Binary(Binary::new(
                    clock(),
                    RValue::Local(saved.clone()),
                    BinaryOperation::Sub,
                ))],
            )),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&saved), "lastTime");
    }

    #[test]
    fn non_clock_subtraction_base_is_not_named_last_time() {
        let duration = RcLocal::default();
        let clock = || {
            call(
                RValue::Index(Index::new(global("os"), string("clock"))),
                vec![],
            )
        };
        let mut block = Block(vec![
            declare(&duration, number(5.0)),
            Statement::Call(Call::new(
                global("print"),
                vec![RValue::Binary(Binary::new(
                    clock(),
                    RValue::Local(duration.clone()),
                    BinaryOperation::Sub,
                ))],
            )),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&duration), "v");
    }

    #[test]
    fn multi_result_overwrite_invalidates_last_time() {
        let ignored = RcLocal::default();
        let saved = RcLocal::default();
        let clock = || {
            call(
                RValue::Index(Index::new(global("os"), string("clock"))),
                vec![],
            )
        };
        let mut block = Block(vec![
            declare(&saved, clock()),
            Statement::Assign(Assign::new(
                vec![LValue::Local(ignored), LValue::Local(saved.clone())],
                vec![RValue::Select(Select::Call(Call::new(
                    global("getState"),
                    vec![],
                )))],
            )),
            Statement::Call(Call::new(
                global("print"),
                vec![RValue::Binary(Binary::new(
                    clock(),
                    RValue::Local(saved.clone()),
                    BinaryOperation::Sub,
                ))],
            )),
        ]);
        name_locals(&mut block, true);
        assert_ne!(name_of(&saved), "lastTime");
    }

    #[test]
    fn increment_only_local_named_count() {
        let value = RcLocal::default();
        let mut block = Block(vec![
            declare(&value, number(0.0)),
            Statement::Assign(Assign::new(
                vec![LValue::Local(value.clone())],
                vec![RValue::Binary(Binary::new(
                    RValue::Local(value.clone()),
                    number(1.0),
                    BinaryOperation::Add,
                ))],
            )),
            use_local(&value),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&value), "count");
    }

    #[test]
    fn guarded_boolean_cell_named_flag() {
        let value = RcLocal::default();
        let mut block = Block(vec![
            declare(&value, boolean(false)),
            Statement::If(If::new(
                RValue::Local(value.clone()),
                Block(vec![Statement::Assign(Assign::new(
                    vec![LValue::Local(value.clone())],
                    vec![boolean(true)],
                ))]),
                Block::default(),
            )),
            use_local(&value),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&value), "flag");
    }

    #[test]
    fn connection_map_named_from_value_and_key_roles() {
        let map = RcLocal::default();
        let event = RValue::Index(Index::new(global("button"), string("Activated")));
        let connection = RValue::MethodCall(MethodCall::new(
            event,
            "Connect".to_string(),
            vec![global("handler")],
        ));
        let mut block = Block(vec![
            declare(&map, RValue::Table(Table::default())),
            Statement::Assign(Assign::new(
                vec![LValue::Index(Index::new(
                    RValue::Local(map.clone()),
                    RValue::Index(Index::new(global("Players"), string("LocalPlayer"))),
                ))],
                vec![connection],
            )),
            use_local(&map),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&map), "connectionsByLocalPlayer");
    }

    #[test]
    fn unknown_map_key_prevents_partial_by_suffix() {
        let map = RcLocal::default();
        let connection = || {
            RValue::MethodCall(MethodCall::new(
                global("event"),
                "Connect".to_string(),
                vec![global("handler")],
            ))
        };
        let mut block = Block(vec![
            declare(&map, RValue::Table(Table::default())),
            Statement::Assign(Assign::new(
                vec![LValue::Index(Index::new(
                    RValue::Local(map.clone()),
                    RValue::Index(Index::new(global("Players"), string("LocalPlayer"))),
                ))],
                vec![connection()],
            )),
            Statement::Assign(Assign::new(
                vec![LValue::Index(Index::new(
                    RValue::Local(map.clone()),
                    call(global("getKey"), vec![]),
                ))],
                vec![connection()],
            )),
            use_local(&map),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&map), "connections");
    }

    #[test]
    fn generic_map_key_is_not_used_as_by_suffix() {
        let map = RcLocal::default();
        let key = named_local("k");
        let connection = RValue::MethodCall(MethodCall::new(
            global("event"),
            "Connect".to_string(),
            vec![global("handler")],
        ));
        let mut block = Block(vec![
            declare(&map, RValue::Table(Table::default())),
            Statement::Assign(Assign::new(
                vec![LValue::Index(Index::new(
                    RValue::Local(map.clone()),
                    RValue::Local(key),
                ))],
                vec![connection],
            )),
            use_local(&map),
        ]);
        name_locals(&mut block, true);
        assert_eq!(name_of(&map), "connections");
    }

    #[test]
    fn pluralizer_preserves_existing_plural() {
        assert_eq!(pluralize("points").as_deref(), Some("points"));
        assert_eq!(pluralize("models").as_deref(), Some("models"));
        assert_eq!(pluralize("status").as_deref(), Some("statuses"));
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
                    vec![method_call(
                        global("inst"),
                        "GetAttribute",
                        vec![string("OwnerId")],
                    )],
                ),
            ),
            use_local(&a),
            // tonumber(x, 16): 2 args -> no name signal -> stays default.
            declare(
                &b,
                call(global("tonumber"), vec![global("x"), number(16.0)]),
            ),
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
