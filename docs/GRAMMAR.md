# kaish Formal Grammar

This document defines kaish syntax in a way that's:
1. Unambiguous (every input has exactly one parse, or is an error)
2. LL(k) friendly (predictable with limited lookahead)
3. Testable (every production rule can be exercised in isolation)

## Design Principles for Testability

### 1. Every Construct Has a Unique Leading Token

```
set NAME = ...     # assignment starts with 'set'
tool NAME ...      # tool def starts with 'tool'
if COND ...        # conditional starts with 'if'
for VAR ...        # loop starts with 'for'
IDENT ...          # command starts with identifier
```

No ambiguity about what we're parsing after seeing the first token.

### 2. Explicit Delimiters Everywhere (JSON Only)

```
${VAR}                    # variables always braced
{body}                    # tool bodies in braces (newline after {)
["a", "b", "c"]           # arrays - JSON syntax
{"key": "val"}            # objects - JSON syntax
"string"                  # strings always quoted
```

**Core parser is JSON-only** for structured data. No YAML-lite ambiguity.
REPL provides expansion: type `{host: localhost}<TAB>` → `{"host": "localhost"}`

### 3. No Implicit Operations

```bash
# Bash: implicit word splitting, glob expansion
echo $FILES        # might expand to multiple args!

# kaish: explicit everything
echo ${FILES}      # interpolates to single value
glob pattern="*.rs" | scatter | process ${ITEM}  # explicit glob tool
```

### 4. Whitespace Rules Are Simple

- Whitespace separates tokens
- Newlines are significant (end statements, except after `\` or inside `{}[]`)
- Inside quotes, whitespace is literal
- That's it. No magic.

---

## EBNF Grammar

```ebnf
(* === Top Level === *)

program     = { statement } ;
statement   = assignment
            | tool_def
            | if_stmt
            | for_stmt
            | pipeline
            | NEWLINE
            ;

(* === Statements === *)

assignment  = "set" , IDENT , "=" , value ;

tool_def    = "tool" , IDENT , { param_def } , "{" , { statement } , "}" ;
param_def   = IDENT , ":" , TYPE , [ "=" , literal ] ;

if_stmt     = "if" , condition , ";" , "then" , { statement } ,
              [ "else" , { statement } ] , "fi" ;

for_stmt    = "for" , IDENT , "in" , value , ";" , "do" , { statement } , "done" ;

pipeline    = command , { "|" , command } , [ "&" ] , [ redirect ] ;

command     = IDENT , { argument } ;

(* === Arguments === *)

argument    = positional | named ;
positional  = value ;
named       = IDENT , "=" , value ;

(* === Values === *)

value       = literal
            | var_ref
            | cmd_subst
            | interpolated_string
            ;

literal     = STRING
            | INT
            | FLOAT
            | BOOL
            | array
            | object
            ;

cmd_subst   = "$(" , pipeline , ")" ;

var_ref     = "${" , var_path , "}" ;
var_path    = IDENT , { "." , IDENT | "[" , INT , "]" } ;

interpolated_string = '"' , { string_part } , '"' ;
string_part = CHARS | var_ref ;

array       = "[" , [ value , { "," , value } ] , "]" ;
object      = "{" , [ pair , { "," , pair } ] , "}" ;
pair        = STRING , ":" , value ;   (* JSON style: keys must be quoted *)

(* === Redirects === *)

redirect    = redir_op , value ;
redir_op    = ">" | ">>" | "<" | "2>" | "&>" ;

(* === Conditions === *)

(* Conditions are expressions, not commands. Use $(cmd) for command exit checks. *)
(* Logical operators && and || work on expression results, not statement chaining. *)

condition   = or_expr ;
or_expr     = and_expr , { "||" , and_expr } ;
and_expr    = cmp_expr , { "&&" , cmp_expr } ;
cmp_expr    = value , [ comp_op , value ] ;
comp_op     = "==" | "!=" | "<" | ">" | "<=" | ">=" ;

(* === Tokens (Lexer) === *)

IDENT       = ALPHA , { ALPHA | DIGIT | "-" | "_" } ;
STRING      = '"' , { CHAR | ESCAPE | var_ref } , '"' ;
INT         = [ "-" ] , DIGIT , { DIGIT } ;
FLOAT       = INT , "." , DIGIT , { DIGIT } ;
BOOL        = "true" | "false" ;
TYPE        = "string" | "int" | "float" | "bool" | "array" | "object" ;

ALPHA       = "a"-"z" | "A"-"Z" | "_" ;
DIGIT       = "0"-"9" ;
CHAR        = (* any char except " \ ${ *) ;
ESCAPE      = "\" , ( '"' | "\" | "n" | "t" | "r" | "u" , HEX , HEX , HEX , HEX ) ;
HEX         = DIGIT | "a"-"f" | "A"-"F" ;

NEWLINE     = "\n" | "\r\n" ;
COMMENT     = "#" , { (* any char except newline *) } ;
```

---

## Ambiguity Analysis

### Potential Ambiguity 1: Object vs Tool Body

Both use `{...}`. Resolution: **context**.

```bash
tool foo { ... }           # after 'tool IDENT', brace = body
cmd config={key: val}      # after '=', brace = object literal
```

Lexer hint: after `tool IDENT`, switch to "expecting body" mode.
Or: tool bodies use different delimiters?

**Option A**: Keep `{}` for both, parser tracks context
**Option B**: Tool bodies use `do...end` or indentation
**Option C**: Tool bodies use `{| ... |}` or similar

I lean toward **Option A** (context) since it's familiar, but let's flag it.

### Potential Ambiguity 2: Bare Words vs Keywords

```bash
set = "value"    # is 'set' a command or keyword?
if = 5           # is 'if' a command or keyword?
```

Resolution: **keywords are reserved**. You cannot have a command named `set`, `tool`, `if`, `for`, `then`, `else`, `fi`, `do`, `done`.

### Potential Ambiguity 3: Named Args vs Positional Comparison

```bash
cmd foo=bar      # named arg
cmd foo = bar    # three positional args? or error?
```

Resolution: **no spaces around `=` in named args**. With spaces, it's positional args.

Actually, let's be stricter: **spaces around `=` only in `set` statements**.

```bash
set X = "value"  # assignment (spaces required)
cmd foo=bar      # named arg (no spaces allowed)
cmd foo = bar    # ERROR: unexpected '=' in arguments
```

This removes ambiguity entirely.

### Potential Ambiguity 4: Value Types (RESOLVED: JSON-only)

With JSON-only syntax, there's no ambiguity:

```bash
cmd flag=true                    # ✅ bool (JSON literal)
cmd flag=false                   # ✅ bool (JSON literal)
cmd count=123                    # ✅ int (JSON number)
cmd count=123.45                 # ✅ float (JSON number)
cmd name="foo"                   # ✅ string (JSON string)
cmd items=["a", "b"]             # ✅ array (JSON array)
cmd config={"host": "localhost"} # ✅ object (JSON object)
```

Invalid (not JSON):
```bash
cmd flag=yes       # ❌ ERROR: unexpected identifier, use true or "yes"
cmd flag=YES       # ❌ ERROR: unexpected identifier, use true or "YES"
cmd name=foo       # ❌ ERROR: unexpected identifier, use "foo"
cmd config={host: "x"}  # ❌ ERROR: object keys must be quoted
```

**REPL convenience**: Tab-expansion converts relaxed input to valid JSON.
```
会sh> cmd config={host: localhost}<TAB>
会sh> cmd config={"host": "localhost"}
```

---

## Test Categories

### Category 1: Lexer Tests (Token Stream)

For each token type, test:
- Valid examples
- Edge cases
- Invalid examples (should error)

```
┌─────────────────────────────────────────────────────────────────┐
│ Input                    │ Expected Tokens                     │
├─────────────────────────────────────────────────────────────────┤
│ set X = 5                │ SET IDENT(X) EQ INT(5)              │
│ echo "hello"             │ IDENT(echo) STRING(hello)           │
│ echo ${X}                │ IDENT(echo) VARREF(X)               │
│ cmd a=1 b="x"            │ IDENT(cmd) IDENT(a) EQ INT(1) ...   │
│ # comment                │ COMMENT                              │
│ "unterminated            │ ERROR: unterminated string           │
└─────────────────────────────────────────────────────────────────┘
```

### Category 2: Parser Tests (AST Structure)

For each grammar production:
- Minimal valid example
- Complex valid example
- Boundary cases
- Invalid examples (should error with good message)

```
┌─────────────────────────────────────────────────────────────────┐
│ Production    │ Input              │ Expected AST              │
├─────────────────────────────────────────────────────────────────┤
│ assignment    │ set X = 5          │ Assign(X, Int(5))         │
│ assignment    │ set X = [1,2]      │ Assign(X, Array([1,2]))   │
│ assignment    │ X = 5              │ ERROR: use 'set X = ...'  │
│ command       │ echo "hi"          │ Cmd(echo, [Str(hi)])      │
│ pipeline      │ a | b | c          │ Pipe([Cmd(a),Cmd(b),...]) │
│ named_arg     │ foo=123            │ Named(foo, Int(123))      │
│ named_arg     │ foo = 123          │ ERROR: no spaces in arg   │
└─────────────────────────────────────────────────────────────────┘
```

### Category 3: Round-Trip Tests (Parse → Print → Parse)

Property: `parse(print(parse(input))) == parse(input)`

Generate random valid ASTs, pretty-print them, parse again, compare.

### Category 4: Evaluation Tests (Input → Output)

Golden file tests: script + expected stdout/stderr/exit code.

```
┌─────────────────────────────────────────────────────────────────┐
│ Script                           │ Expected                    │
├─────────────────────────────────────────────────────────────────┤
│ echo "hello"                     │ stdout: hello               │
│ set X = 5; echo ${X}             │ stdout: 5                   │
│ echo ${UNDEFINED}                │ stderr: undefined var       │
│ false && echo "no"               │ stdout: (empty)             │
│ false || echo "yes"              │ stdout: yes                 │
└─────────────────────────────────────────────────────────────────┘
```

### Category 5: Error Message Tests

Every error path should have a test verifying the message is helpful.

```
┌─────────────────────────────────────────────────────────────────┐
│ Input                │ Expected Error Contains                  │
├─────────────────────────────────────────────────────────────────┤
│ set X = yes          │ "ambiguous value 'yes', use true or"     │
│ echo ${X.y.z         │ "unterminated variable reference"        │
│ tool { }             │ "expected tool name after 'tool'"        │
│ cmd foo = bar        │ "unexpected '=' - named args use 'k=v'"  │
└─────────────────────────────────────────────────────────────────┘
```

### Category 6: Fuzz Tests

Throw random bytes at the parser. It should:
- Never panic
- Never hang
- Always return `Ok(AST)` or `Err(ParseError)`

Use `cargo-fuzz` or `proptest` with arbitrary byte sequences.

---

## Test File Format

For golden file tests, I propose a simple format:

```
# test: name_of_test
# description: What this tests
---
set X = 5
echo ${X}
---
stdout: 5
exit: 0
===

# test: error_undefined_var
# expect: error
---
echo ${NOPE}
---
error: undefined variable 'NOPE'
===
```

This gives us:
- Human-readable test files
- Easy to add new tests (copy-paste-modify)
- Parseable by test harness
- Self-documenting

---

## Property-Based Testing Ideas

Using `proptest` or `quickcheck`:

```rust
// Property 1: Parsing never panics
proptest! {
    fn parse_never_panics(input: String) {
        let _ = parse(&input); // should return Ok or Err, never panic
    }
}

// Property 2: Valid AST round-trips
proptest! {
    fn valid_ast_roundtrips(ast: Ast) {
        let printed = pretty_print(&ast);
        let reparsed = parse(&printed).unwrap();
        assert_eq!(ast, reparsed);
    }
}

// Property 3: Token stream is deterministic
proptest! {
    fn lexer_deterministic(input: String) {
        let tokens1 = lex(&input);
        let tokens2 = lex(&input);
        assert_eq!(tokens1, tokens2);
    }
}
```

---

## Command Substitution `$(cmd)`

Command substitution runs a command and returns its structured result:

```bash
# In conditions - check if command succeeded
if $(validate input.json); then
    echo "valid"
fi

# With field access - check specific result field
if $(validate input.json).ok; then
    echo "valid"
fi

# Capture result for later use
set RESULT = $(fetch url="https://api.example.com")
echo ${RESULT.data}

# Logical operators work on expression results
if $(check-a) && $(check-b); then
    echo "both passed"
fi

if $(try-primary) || $(try-fallback); then
    echo "at least one worked"
fi
```

The `$(cmd)` expression evaluates to the command's result object (same structure as `$?`):
- `.ok` - bool: true if exit code 0
- `.code` - int: exit code
- `.data` - object: parsed stdout (if JSON)
- `.out` - string: raw stdout
- `.err` - string: error message

This design keeps conditions as pure expressions (no hidden side effects) while making
command execution explicit. The `&&` and `||` operators work on expression results,
avoiding bash's ambiguous statement chaining semantics.

## Deferred Features: Future Compatibility

Explicitly not in v0.1, designed for future addition:

### Object Destructuring in Scatter

Current: `scatter as=VAR`
Future: `scatter as={id, url}` or `scatter as={id: ID, url: URL}`

```ebnf
pattern       = IDENT | "{" , pattern_field , { "," , pattern_field } , "}" ;
pattern_field = IDENT [ ":" , IDENT ] ;
```

Unambiguous: `{` after `as=` starts a pattern (unquoted idents), not JSON object.
