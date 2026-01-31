---
description: Create a well-structured GitHub issue from a description
argument-hint: <description of the issue to create>
allowed-tools: [Bash, Read, Glob, Grep, WebFetch]
---

# Create GitHub Issue

Create a GitHub issue based on the user's description: $ARGUMENTS

## Issue quality requirements

Every issue you create must include the following sections:

### Summary
A clear description of what needs to be done and why. Include enough context that someone unfamiliar with the codebase (a person or an AI agent) can understand the motivation and scope.

### Current state
Describe the relevant current behaviour or architecture. Reference specific files and code paths. This grounds the issue in the actual codebase rather than abstract descriptions.

### Desired behaviour
Describe what the system should do after the issue is resolved. Be specific about the expected outcomes.

### Key files
List the files that are most likely to need changes. Use `Grep` and `Glob` to find the actual relevant files in the codebase rather than guessing. Every file reference should be a real path that exists in the repo.

### Testing
Every issue MUST include a testing section. Describe specific tests that should be written or updated:

- **Unit tests**: What functions or modules need test coverage? What edge cases should be tested?
- **Integration tests**: Are there existing test suites (kernel tests, userspace tests) that should be extended? Should a new test be added to `KERNEL_TESTS` or `USERSPACE_TESTS` in the Makefile?
- **Regression tests**: What existing tests should still pass after the change?

Be concrete: "Add a kernel test that calls X and verifies Y" is better than "Add tests".

### Documentation
Every issue MUST include a documentation section. Describe what documentation should be written or updated:

- **Code documentation**: Which public APIs, traits, or structs need doc comments?
- **Architecture documentation**: Do any docs in `docs/` need updating?
- **Inline comments**: Are there complex algorithms or non-obvious design decisions that need explanation?

If the change is purely internal with no public API, say so explicitly rather than omitting the section.

## Process

1. Read the user's description carefully. Ask clarifying questions if the scope is ambiguous.
2. Search the codebase to understand the current state. Use `Grep` and `Glob` to find relevant files, types, and functions. Read key files to understand the existing architecture.
3. Determine appropriate labels. Use existing labels from the repo. Check available labels with `gh label list`.
4. If the issue is part of a larger epic, note the parent issue number and use `Part of #N` at the end of the body.
5. Create the issue with `gh issue create`.
6. If the user asked for sub-issues, link them to the parent using the GraphQL API:
   - Get the parent issue node ID: `gh api graphql -f query='{ repository(owner: "OWNER", name: "REPO") { issue(number: N) { id } } }'`
   - Get the child issue node ID similarly
   - Link: `gh api graphql -f query='mutation { addSubIssue(input: { issueId: "PARENT_ID", subIssueId: "CHILD_ID" }) { issue { id } subIssue { id } } }'`
7. Report the created issue URL back to the user.

## Label guidelines

- `bug` — something is broken or incorrect
- `enhancement` — new feature or improvement
- `documentation` — documentation-only changes
- `technical-debt` — code quality and maintainability
- `security` — security-related changes
- `performance` — performance improvements
- `kernel` — kernel-space changes
- `userspace` — userspace libraries and programs
- `drivers` — device drivers
- `filesystem` — VFS, ext2, tarfs
- `ipc` — inter-process communication
- `syscall` — syscall ABI and implementation
- `good first issue` — suitable for newcomers

Apply multiple labels where appropriate. Every issue should have at least one category label (bug/enhancement/documentation/technical-debt) and at least one component label (kernel/userspace/drivers/filesystem/ipc/syscall).

## Style

- Use GitHub-flavoured markdown
- Use code blocks for file paths, function names, and code snippets
- Reference other issues with `#N` syntax
- Keep the summary concise but the implementation details thorough
- Use imperative mood for the title (e.g., "Add X support" not "Adding X support")
