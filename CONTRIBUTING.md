# CONTRIBUTING

Contributions are appreciated! Follow these guidelines:

- Check your code with the linter by running `make clippy`
- Format your code using `make fmt`
- Ensure all tests pass with `make check`

## Code style

The following code guidelines will help make code review smoother.

### Use of `unwrap` and `expect`

Use only in the following cases:

1.  It's impossible for the `Result` to be an `Err`, eg. when parsing a string
    into a type that is known to be valid.
2.  Test code, where panicking is acceptable.

In either case, add a comment explaining why it's safe to `unwrap` or `expect`
with a comment starting with `// SAFETY: ...`.

### Module imports

Modules are declared at the top of the file, before the imports. Public modules
are separated from private modules with a blank line:

    mod git;
    mod storage;

    pub mod refs;

    use std::time;
    use std::process;

    ...

Imports are organized in groups, from least specific to more specific:

    use std::collections::HashMap;   // First, `std` imports.
    use std::process;
    use std::time;

    use git_ref_format as format;    // Then, external dependencies.
    use once_cell::sync::Lazy;

    use crate::crypto::PublicKey;    // Finally, local crate imports.
    use crate::storage::refs::Refs;
    use crate::storage::RemoteId;

## Documentation

Public types and functions should be documented with doc comments. Use `///` for
doc comments, and `//!` for module-level documentation.

## Writing commit messages

Commit messages in Barq follow a structured format:

```text
<type>(<scope>): <subject>
```

- **Type:** Indicates the kind of change (e.g., feat, fix, deprecate, remove).
- **Scope:** Optional; specifies the component affected by the change (e.g., common, plugin, docs, github)
- **Subject:** A brief, imperative description of the change.
  - Use present tense, imperative mood (e.g., "change", not "changed" or "changes"), and avoid ending with a dot (.)

Example:

```
feat(common): add Dijkstra's routing algorithm
```
