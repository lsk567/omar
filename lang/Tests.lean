import Omar.Compiler

open Omar

def assertEqual [BEq α] [ToString α] (label : String) (actual expected : α) : IO Unit :=
  if actual == expected then pure ()
  else throw (IO.userError s!"{label}: expected {expected}, got {actual}")

def main : IO UInt32 := do
  try
    let source ← IO.FS.readFile "../tests/topology/HR.omar"
    let program ← match lex source >>= parse with
      | .ok program => pure program
      | .error message => throw (IO.userError message)
    assertEqual "team" program.team "HR"
    assertEqual "agent count" program.agents.size 3
    assertEqual "port count" program.ports.size 6
    assertEqual "reaction count" program.reactions.size 4
    let bytecode ← match compileSource source with
      | .ok bytecode => pure bytecode
      | .error message => throw (IO.userError message)
    match bytecode.getObjVal? "instructions" with
    | .ok (.arr instructions) => assertEqual "instruction count" instructions.size 26
    | _ => throw (IO.userError "compiler did not emit an instruction array")
    IO.println "HR.omar compiler test passed"
    pure 0
  catch error =>
    IO.eprintln error.toString
    pure 1
