# Gateway architecture

The gateway has three deliberately separate boundaries:

1. Listener lifecycle owns binding, coordinated shutdown, drain deadlines,
   and failure propagation across configured listeners.
2. Authentication owns bearer-token comparison and OAuth runtime state. Public
   OAuth metadata and health endpoints do not pass through project auth.
3. `GatewayService` owns project selection and backend execution. HTTP and MCP
   handlers call the same service methods, so authorization and project
   semantics cannot diverge between protocols.

The request path is:

```text
listener -> route/auth boundary -> protocol adapter -> GatewayService
         -> project backend -> versioned response/error envelope
```

The Worker gateway mirrors this arrangement with HTTP routing, auth, services,
and repository modules. Contract fixtures under `schemas/v1/` are shared by
the CLI, Rust HTTP services, and Worker conformance tests.
