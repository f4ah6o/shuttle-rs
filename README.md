# shuttle-rs

`shuttle-rs` is a local-first event log for agent memory, messaging, repository
context, and coordination. The `stl` CLI stores data in `.shuttle/shuttle.db`
under the current Git repository.

## Phase 1 Commands

Initialize local storage:

```bash
stl init
```

Capture generic and typed memories:

```bash
stl remember "SQLite is the local event store"
stl decide "SQLite remains the default because it is local-first"
stl observe "The current branch changes recall ranking"
stl pattern "Use event projections rather than separate state tables"
stl fact "The event log is append-only"
stl bug "Recall ordering should prefer same-repo decisions"
```

Recall entries with ranked results:

```bash
stl recall "SQLite decision"
stl recall "SQLite decision" --type decision
stl --json recall "SQLite decision"
```

Read repository context:

```bash
stl context
stl context --repo
stl context --branch
stl --json context
```

When commands run inside a Git repository, Shuttle attaches repository metadata
to captured events: repository path, remote-derived repository id when present,
branch, commit, dirty status, and dirty file names.
