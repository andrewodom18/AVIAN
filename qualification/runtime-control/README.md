# Runtime control checks

Requires Docker, Python 3, a pinned patched Linux engine build with `build.json`,
and Linux `aviand` and `avianctl` binaries. The runner verifies engine hashes and
records engine and service binary hashes plus runtime configuration in
`provenance.json`.

```sh
python3 qualification/runtime-control/run.py \
  --engine-directory .local/topology-engine-release-candidate \
  --service-directory .local/runtime-service-bin \
  --output qualification/results/runtime-control
```

The runner executes 16 named gates: unauthorized start rejection, engine readiness,
and repeated start for both nodes; overlay ping and TCP echo; live route reporting;
explicit stop; CLI restart and status; delivery after restart; signed revocation
publication into both engines; bidirectional rejection after revocation; and child
cleanup on graceful manager shutdown. Results are written to `runtime-control.json`.
A failed, missing, or duplicated gate returns a nonzero exit status.

Add `--priority-forwarding` when using an engine build that includes that patch.
The flag enables the runtime setting and is recorded in the report and provenance.
Add `--services` to run four additional gates for authenticated local publication,
signed owner announcements exchanged across the overlay, and removal on revocation.
The revocation check issues a signed administrator update, atomically replaces
both peer membership files with mode `0600`, waits for both supervisors to publish
the remaining active credential, and verifies repeated overlay pings are blocked.

Tests run in disposable Linux namespaces inside a privileged container with
`--network none`. Private state uses a temporary filesystem. These commands do not
change host interfaces or routes, connect to ARC or drones, or establish native
macOS, Windows, or Android qualification. Reports remain unqualified for cutover.
