# OMAR Language Specification

Status: draft  
Implementation language: Lean 4  
Source extension: `.omar`

## 1. Purpose

OMAR is a declarative language for agent workflows. A program defines a team of
agents, typed data ports, internal actions, and prompts that react to data.

Agents perform the work described by prompts. The OMAR runtime handles topology
construction, agent lifecycle, routing, scheduling, validation, and topology
mutation programmatically. These orchestration decisions do not use AI.

## 2. Syntax

```ebnf
program     = "team", identifier, "(", agents, ")",
              "{", { declaration }, "}" ;

agents      = [ agent, { ",", agent } ] ;
agent       = identifier, ":", identifier ;

declaration = input | output | action | prompt ;
input       = "input", identifier, ":", type ;
output      = "output", identifier, ":", type ;
action      = "action", identifier, [ ":", type ] ;

prompt      = "prompt", identifier, "(", dependencies, ")",
              "->", productions, prompt-string ;
dependencies = [ identifier, { ",", identifier } ] ;

productions = production, { ",", production } ;
production  = atom, [ "?" ]
            | "(", atom, { "|", atom }, ")", [ "?" ] ;
atom        = identifier, [ "=", literal ] ;

type        = "bool" | "int" | "float" | "string" | "path" | "bytes"
            | "list", "<", type, ">"
            | "option", "<", type, ">" ;
```

Identifiers are case-sensitive. `//` introduces a line comment and `/* ... */`
introduces a block comment.

### 2.1 Team and agents

```omar
team Example(worker : Codex, reviewer : ClaudeCode) {
    // declarations
}
```

The team name identifies the topology. Each agent has a unique name and a
backend. Backend names are resolved by the runtime.

### 2.2 Ports and actions

```omar
input request : string
output result : string
action reviewed : bool
action ready
```

- `input` declares a typed external input.
- `output` declares a typed external result.
- `action` declares an internal event.
- An action without a type is a signal carrying no value.

Input, output, and action names share one namespace and must be unique.

### 2.3 Prompts

```omar
prompt worker(request) -> result
"
    Process $(request).
"
```

The name after `prompt` selects an agent. Parameters name the input or action
values required to trigger the prompt. All dependencies must be available
before the prompt runs.

The quoted body is delivered to the agent. `$(name)` interpolates a parameter.
Interpolation may only reference parameters of that prompt.

### 2.4 Production contracts

The expression after `->` declares what a prompt may produce:

```omar
-> result
-> result, log?
-> (continue_work | accepted = false)
```

- `a, b` requires both productions.
- `(a | b)` requires exactly one production.
- `a?` makes a production optional.
- `a = value` emits a constant value.
- A bare typed target requires the agent to supply a value.
- A bare untyped action emits a signal.

Every production target must be a declared output or action. Produced values
must match the target type.

## 3. Static semantics

A program is valid when:

1. all names are unique in their namespaces;
2. every prompt names a declared agent;
3. every dependency names an input or action;
4. every interpolation names a dependency;
5. every production names an output or action;
6. constant productions match their target types; and
7. all inferred connections join compatible types.

The compiler lowers valid source into a canonical typed topology containing:

```text
Team
Agents
Ports
Reactions
Channels
```

Channels are inferred from ports to dependent prompts and from prompts to their
declared productions.

## 4. Runtime semantics

Submitting an external input creates a flow. Events produced while processing
that input remain associated with the same flow.

A reaction becomes ready when all its dependencies have values in one flow.
When ready, the runtime:

1. renders the prompt;
2. sends it to the selected agent;
3. waits for a structured completion;
4. validates the completion against the production contract; and
5. atomically publishes the resulting events.

The runtime must not infer typed outputs by parsing conversational text. Agents
return outputs through a structured completion operation.

One event may wake several downstream reactions. A reaction with several
dependencies joins values from the same flow. Reactions assigned to one agent
run serially; different agents may run concurrently.

External outputs are observable results. Actions remain internal and wake
downstream reactions. A flow finishes when it has no running or ready reactions
and no pending deliveries.

## 5. Topology identity and mutation

Every team, agent, port, reaction, and channel has a stable component ID derived
from its qualified source name and role. Source ordering does not affect IDs.

The compiler represents a topology canonically so two equivalent programs
produce the same topology hash.

To change a running topology, the reconciler compares the installed topology
with the new desired topology and classifies components as:

```text
retain
create
update
remove
replace
```

Compatible components are retained. In particular, an agent is retained when
its identity and backend are unchanged. Retaining an agent preserves its live
process, conversation context, memory, and current work.

Changing prompts or connections does not replace an otherwise compatible
agent. Changing an agent's identity or backend replaces it.

Mutation follows this order:

1. verify the currently installed revision;
2. create new agents and channels;
3. install new or updated reactions;
4. atomically activate the new topology revision;
5. drain removed connections and reactions; and
6. kill agents that are no longer needed.

An invocation already in progress finishes using the prompt and contract it
started with. New invocations use the new revision.

## 6. Bytecode

The compiler lowers initial deployment and topology diffs to typed bytecode.
Bytecode operations correspond closely to runtime and MCP tool calls.

### 6.1 Topology instructions

```text
BEGIN_PLAN expected_revision desired_hash
SPAWN_AGENT agent_id backend
RETAIN_AGENT agent_id
KILL_AGENT agent_id

DEFINE_PORT port_id kind type
CREATE_CHANNEL channel_id source target
RETAIN_CHANNEL channel_id
DROP_CHANNEL channel_id

INSTALL_REACTION reaction_id agent dependencies contract prompt
UPDATE_REACTION reaction_id contract prompt
REMOVE_REACTION reaction_id

ACTIVATE_REVISION revision
COMMIT_PLAN revision
ABORT_PLAN reason
```

### 6.2 Dataflow instructions

```text
ACCEPT_INPUT port value
EMIT_EVENT flow port value
TRY_ACTIVATE reaction flow
RENDER_PROMPT invocation
DELIVER_PROMPT invocation agent
VALIDATE_COMPLETION invocation result
COMMIT_COMPLETION invocation
FAIL_INVOCATION invocation error
```

### 6.3 Instruction semantics

- `SPAWN_AGENT` calls the MCP agent-spawn capability.
- `KILL_AGENT` calls the MCP agent-kill capability.
- `CREATE_CHANNEL` creates a typed route between two components.
- `INSTALL_REACTION` registers a prompt and its trigger conditions.
- `ACTIVATE_REVISION` atomically switches new inputs to the staged topology.
- `DELIVER_PROMPT` sends the rendered prompt to an existing agent.
- `COMMIT_COMPLETION` atomically publishes all validated outputs.

Instructions are verified before execution. Verification checks operand types,
component existence, channel compatibility, ordering, and revision consistency.

Effectful instructions use stable idempotency keys. The runtime journals each
instruction so it can recover after a crash without spawning duplicate agents
or repeating committed outputs.

If a plan fails before activation, the old topology remains active. If cleanup
fails after activation, the new topology remains active and cleanup is retried.
Retained agents must never be removed during rollback.

## 7. Lean 4 implementation

The intended Lean 4 implementation separates pure compilation from effects:

```lean
parse          : String -> Except ParseError Syntax
elaborate      : Syntax -> Except TypeError Topology
diff           : InstalledTopology -> Topology -> TopologyDiff
compile        : TopologyDiff -> Except CompileError Bytecode
verifyBytecode : Bytecode -> Except VerifyError VerifiedBytecode
execute        : VerifiedBytecode -> RuntimeM Result
```

Parsing, type checking, canonicalization, topology diffing, compilation, and
bytecode verification should be pure. MCP calls, persistence, and process
observation belong to the runtime boundary.

The primary properties to verify are:

- channels connect compatible types;
- bytecode never references an undefined component;
- compatible retained agents are never spawned or killed;
- successful plans produce the desired canonical topology; and
- equal compiler inputs produce equal bytecode.
