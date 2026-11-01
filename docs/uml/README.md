# Anemoi UML

A snapshot of Anemoi's design as of the freeze ([ADR 0001](../adr/0001-freeze-anemoi.md)).
Diagrams were written from the source on `main`, and the class and method names
were checked against it. Unfinished prompt 33 work (intent classification) is not
included.

Each `.puml` file is the source and each `.png` is its render. To regenerate them:

```bash
java -jar plantuml.jar -tpng docs/uml/*.puml
```

## Structure

| Diagram | Shows |
|---|---|
| [Use case](01_use_case.png) | Who uses Anemoi and for what |
| [Package](02_package.png) | Crates and their declared dependencies |
| [Component](03_component.png) | Inside `anemoi-daemon`, with provided and required APIs |
| [Deployment](04_deployment.png) | What runs where, and the bypass path |
| [Class: domain model](05_class_domain.png) | Request, decision and runtime snapshot types |
| [Class: is-a](06_class_is_a.png) | Trait implementations; Adapter and Strategy patterns |
| [Class: has-a](07_class_has_a.png) | What `AppState` owns and shares |
| [Pattern: Simple Factory](08_pattern_factory.png) | How config selects a runtime adapter |
| [Not a pattern: push cache](09_pattern_push_cache.png) | Why the event stream is not Observer |

## Behavior

| Diagram | Shows |
|---|---|
| [Sequence: gateway](10_sequence_gateway.png) | One chat completion, with every refusal path |
| [Sequence: staging](11_sequence_staging.png) | One background staging tick |
| [Communication: reconcile](12_communication_reconcile.png) | One reconciliation tick |
| [Activity: decide](13_activity_decide.png) | `Scheduler::decide`, including continuity staging |
| [State: residency](14_state_residency.png) | Model states observed from llama-swap |
| [State: staging](15_state_staging.png) | `StagingIntent` lifecycle |

## Reading UML against Rust

Rust has no classes or inheritance, so these diagrams use this mapping:

| UML | Rust |
|---|---|
| Class | `struct` with its `impl` block |
| Interface and realization (is-a) | `trait` and `impl Trait for Type` |
| Composition (filled diamond) | A field the owner holds by value |
| Aggregation (open diamond) | A field shared through `Arc` |
| Multiplicity `0..1` / `0..*` | `Option<T>` / `Vec<T>` or `HashMap` |

## Not shown

- **`TransitionCoordinator`** in `anemoi-policy` is implemented and tested, but the
  daemon does not call it, so it appears in no runtime flow.
- **`anemoi-mcp`** is a library with no binary or daemon route. It appears in the
  package diagram but not as a use-case actor.
- Governance and CLI request flows repeat the same `AppState` calls, so they have
  no sequence diagrams of their own.
- Sequence diagrams omit some failure branches, such as an inspect error during
  refresh and a runtime with no `base_url`.
