# Security policy

## Supported versions

ferridriver is pre-1.0. Security fixes land on `main` and ship in the next
release; only the latest release is supported.

## Reporting a vulnerability

Report vulnerabilities privately through
[GitHub security advisories](https://github.com/salamaashoush/ferridriver/security/advisories/new)
rather than opening a public issue. Include the affected version
(`ferridriver --version`), the backend, and a minimal reproduction.

Areas where a report is most valuable:

- Escapes from the `run_script` sandbox: filesystem access outside
  `script_root` or `artifacts_root`, network access past `allow.net`, or
  reaching the host process.
- Caller-supplied `args` being evaluated as script source.
- The MCP HTTP transport accepting requests it should refuse.
