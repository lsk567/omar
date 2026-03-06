Build a compiler for a small statically-typed programming language that compiles to JavaScript.

The language should support: variables with type annotations, functions with generics, if/else, loops, structs, pattern matching, and a small standard library (print, math, string ops).

Phase 1 - Language Design: Design the language syntax and semantics. Write a formal grammar (BNF) and a language spec with examples. Get this right before any implementation.

Phase 2 - Frontend: Build a lexer and parser that produces an AST. The parser should give good error messages with line numbers and suggestions.

Phase 3 - Type System: Build a type checker with type inference, generics support, and struct type checking. Error messages should be helpful (like Rust's).

Phase 4 - Backend: Build an IR, implement at least 3 optimization passes (dead code elimination, constant folding, inlining), and a JavaScript code generator.

Phase 5 - Standard Library: Implement the stdlib in the language itself where possible, with JS FFI for the rest.

Phase 6 - Test Suite: Comprehensive tests for every phase — lexer, parser, type checker, optimizer, codegen. Include error case tests. The test suite should be runnable with a single command.

Phase 7 - Demo Programs: Write at least 5 non-trivial programs in the language (fibonacci, linked list, simple calculator, etc.) that compile and run correctly.

IMPORTANT CONSTRAINTS:
- Each phase depends on the previous one. You cannot start Phase 3 until Phase 2 produces an AST.
- Each phase is complex enough that it should be delegated to a dedicated agent.
- Sub-agents should further decompose their work. For example, the type system agent should have separate sub-agents for inference, generics, and error reporting.
- Aim for at least 3 levels of agent hierarchy below the top-level coordinator.

Put all artifacts under <omar-root>/junk/compiler/.
