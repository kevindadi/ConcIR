# ConcIR EBNF

Abstract syntax of ConcIR in ISO/IEC 14977 EBNF. The concrete syntax is
tagged JSON (`"kind"` discriminators); this grammar is the structure the
deserializer accepts. Well-formedness that serde cannot express (resource
fields by type, FQN vs short name in `provides`/`requires`, E409, …) is
enforced by `src/validate/` — see [`syntax.md`](syntax.md) and
[`error_codes.md`](error_codes.md).

Notation: `=` definition, `,` sequence, `|` choice, `[…]` optional,
`{…}` repetition (zero or more), `(…)` grouping, `"…"` terminal,
`(* … *)` comment, `;` end of rule. `{ X }-` means one or more `X`.

## Lexical

```ebnf
Letter     = "A" | "B" | "C" | "D" | "E" | "F" | "G" | "H" | "I" | "J"
           | "K" | "L" | "M" | "N" | "O" | "P" | "Q" | "R" | "S" | "T"
           | "U" | "V" | "W" | "X" | "Y" | "Z"
           | "a" | "b" | "c" | "d" | "e" | "f" | "g" | "h" | "i" | "j"
           | "k" | "l" | "m" | "n" | "o" | "p" | "q" | "r" | "s" | "t"
           | "u" | "v" | "w" | "x" | "y" | "z"
           | "_" ;

Digit      = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;

Ident      = Letter, { Letter | Digit } ;
               (* [A-Za-z_][A-Za-z0-9_]* ; module / entity / handle names *)

Sid        = "s", Digit, { Digit } ;
               (* "s1", "s10"; first block of a function is the entry *)

Fqn        = Ident, "::", Ident ;
               (* exactly one "::"; crate::foo::bar is not a ConcIR FQN *)

Name       = Ident | Fqn ;
               (* same-module: Ident; cross-module: Fqn listed in requires *)

Location   = Fqn, ".", Sid ;
               (* control place, e.g. core::main.s3 *)

Expr       = ? string ? ;
               (* unparsed expression / guard / literal; subset checked in E2xx / E9xx *)

JsonValue  = ? JSON value ? ;
Integer    = ? JSON integer ? ;
Boolean    = "true" | "false" ;
String     = ? JSON string ? ;
```

Naming constraints (not encoded as extra nonterminals): `provides` uses
`Ident`; `requires` and `entry` use `Fqn`.

## Program and module

```ebnf
Program    = Ident,                     (* program *)
             [ String ],                (* version; default "3.1.0" *)
             { Module }-,               (* modules; at least one *)
             Fqn ;                      (* entry = module::function *)

Module     = Ident,                     (* name *)
             [ NameSet ],               (* provides: short names *)
             [ NameSet ],               (* requires: FQNs *)
             { Resource },
             { Protection },
             { Function } ;

NameSet    = { Ident | Fqn },           (* resources *)
             { Ident | Fqn } ;          (* functions *)
```

## Types

```ebnf
BaseType   = Primitive
           | BoundedInt
           | EnumType
           | StructType
           | ArrayType ;

Primitive  = "Bool" | "Int" | "Float" | "String" ;

BoundedInt = "Int", Integer, Integer ;  (* [lo, hi], lo <= hi *)

EnumType   = { Ident }- ;               (* variants *)

StructType = { Ident, BaseType }- ;     (* field : type *)

ArrayType  = BaseType, Integer ;        (* elem, len *)
```

## Resource and protection

```ebnf
Resource   = Ident,                     (* name *)
             ResKind,
             ResType,
             [ Mode ],
             [ Integer ],               (* count; required for Semaphore *)
             [ BaseType ],              (* required for Channel, Var, Atomic *)
             [ JsonValue ],             (* init; required for Var, Atomic *)
             [ Integer ] ;              (* capacity; required for Channel *)

ResKind    = "sync" | "var" ;

ResType    = "Mutex" | "RwLock" | "Condvar" | "Semaphore" | "Channel"
           | "Var" | "Atomic" ;

Mode       = "Sync" | "Async" ;

Protection = Ident, Ident ;             (* var, lock; Var only, not Atomic *)
```

Well-formed resources (validator E001 / E008 / E009):

```ebnf
SyncRes    = MutexRes | RwLockRes | CondvarRes | SemaphoreRes | ChannelRes ;
MutexRes   = Ident, "sync", "Mutex", Mode ;
RwLockRes  = Ident, "sync", "RwLock", Mode ;
CondvarRes = Ident, "sync", "Condvar", Mode ;
SemaphoreRes
           = Ident, "sync", "Semaphore", Mode, Integer ;
ChannelRes = Ident, "sync", "Channel", Mode, BaseType, Integer ;
               (* capacity: 0 = rendezvous; n ≥ 1 = n payload slots of base *)

VarRes     = Ident, "var", "Var", BaseType, JsonValue ;
AtomicRes  = Ident, "var", "Atomic", BaseType, JsonValue ;
```

## Function

```ebnf
Function   = Ident,                     (* name *)
             FnKind,
             { ParamDecl },             (* params *)
             [ ParamDecl ],             (* returns *)
             { LocalDecl },             (* locals *)
             { Block },                 (* body; empty = nobody placeholder *)
             [ Effects ] ;

FnKind     = "normal" | "async" | "closure" ;

ParamDecl  = Ident, BaseType, Boolean ; (* name, type, modeled *)

LocalDecl  = Ident, BaseType, Boolean,  (* name, type, modeled *)
             [ JsonValue ] ;            (* init *)

Effects    = { Name },                  (* reads *)
             { Name } ;                 (* writes; nobody-function hint *)
```

## Basic block (flattened CFG)

A block is statements then exactly one terminator. There is no block-level
`call` field and no `loop` statement. Loops are `Branch` back-edges.

```ebnf
Block      = Sid,
             { Stmt },
             Terminator ;
```

## Statement

Statements do not transfer control. `Name` is a resource or function
reference (short or FQN). `Ident` on the left of `assign_local` is a local.

```ebnf
Stmt       = Nop
           | AssignLocal
           | ReadShared
           | WriteShared
           | AbstractStep
           | AtomicLoad
           | AtomicStore
           | AtomicCas
           | MutexLock
           | MutexUnlock
           | RwLockRead
           | RwLockWrite
           | RwLockUnlock
           | ChannelSend
           | ChannelRecv
           | CondvarWait
           | CondvarNotify
           | CondvarNotifyAll
           | SemaphoreAcquire
           | SemaphoreRelease
           | Call
           | Spawn
           | SpawnBatch
           | Join
           | JoinAll
           | AsyncCall
           | Await ;

Nop        = "nop" ;

AssignLocal
           = "assign_local", Ident, Expr ;

ReadShared = "read_shared", Name, [ Ident ] ;
               (* resource, optional dst *)

WriteShared
           = "write_shared", Name, Expr ;

AbstractStep
           = "abstract_step", { Name }, { Name }, [ String ] ;
               (* reads, writes, desc *)

AtomicLoad = "atomic_load", Name, Ident ;
               (* dst := current value, Atomic base type *)
AtomicStore
           = "atomic_store", Name, Expr ;
AtomicCas  = "atomic_cas", Name, Expr, Expr, Ident ;
               (* resource, expected, desired, dst.
                  dst := pre-CAS old value, same type as Atomic base,
                  not a Bool success flag. Success is dst == expected. *)

MutexLock  = "mutex_lock", Name ;
MutexUnlock
           = "mutex_unlock", Name ;

RwLockRead = "rwlock_read", Name ;
RwLockWrite
           = "rwlock_write", Name ;
RwLockUnlock
           = "rwlock_unlock", Name ;

ChannelSend
           = "channel_send", Name, Expr ;
ChannelRecv
           = "channel_recv", Name, Ident ;
               (* channel, dst; dst := popped payload of Channel.base;
                  "_" discards. Buffer is the Channel's capacity slots. *)

CondvarWait
           = "condvar_wait", Name, Name ;
               (* condvar, lock *)
CondvarNotify
           = "condvar_notify", Name ;
CondvarNotifyAll
           = "condvar_notify_all", Name ;

SemaphoreAcquire
           = "semaphore_acquire", Name, [ Integer ] ;
SemaphoreRelease
           = "semaphore_release", Name, [ Integer ] ;

Call       = "call", Name, { Expr }, [ Name ] ;
               (* func, args, optional dst *)

Spawn      = "spawn", Name, { Expr }, Ident ;
               (* func, args, handle *)
SpawnBatch = "spawn_batch", Name, Integer, Ident ;
Join       = "join", Ident ;
JoinAll    = "join_all", Ident ;

AsyncCall  = "async_call", Name, { Expr }, Ident ;
Await      = "await", Ident ;
```

## Terminator

The only place successors and `return` appear.

```ebnf
Terminator = Goto
           | Branch
           | Switch
           | Return
           | Select ;

Goto       = "goto", Sid ;

Branch     = "branch", Expr, Sid, Sid ;
               (* cond, then, else; a Sid earlier in the body is a back-edge *)

Switch     = "switch", Name, { String, Sid }-, Sid ;
               (* var, cases (label → sid)+, default *)

Return     = "return", [ Expr ] ;

Select     = "select", { SelectBranch }-, [ Sid ] ;
               (* branches, optional default *)

SelectBranch
           = SelectGuard, Sid ;

SelectGuard
           = ChannelRecvGuard
           | CondvarWaitGuard
           | SemaphoreAcquireGuard ;

ChannelRecvGuard
           = ChannelRecv ;
               (* same fields as the statement, including dst *)

CondvarWaitGuard
           = "condvar_wait", Name, Name ;
               (* legal only if the function is async and the Condvar is
                  mode Async (E409); translator maps to Notify / watch *)

SemaphoreAcquireGuard
           = "semaphore_acquire", Name ;
```
