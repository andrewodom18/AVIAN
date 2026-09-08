# ARC single-plug radio swarm onboarding

## Ownership

ARC owns the guided operator workflow and its durable journal. CHUD remains the only service that discovers, identifies, snapshots, writes, confirms, and reads physical radios. AVIAN supplies planning validation and PEAT-backed logical mesh observations; it does not call CHUD or implement vendor writes.

Saved ARC device/drone connections and MAC-keyed radio-swarm records appear together under **Saved Devices / Drones**, but remain separate data types. ARC does not infer that a physical radio belongs to a saved drone without an explicit association.

## Workflow

1. Select **+ Add Swarm** beside **+ Add Device**.
2. Define the swarm name, node-name prefix, and optional expected radio count from 1 through 200. When supplied, the expected count is a completion target: ARC will not finish the swarm until at least that many radios are verified.
3. Plug in one radio.
4. ARC polls CHUD inventory. Configuration remains unavailable until CHUD reports the radio confirmed or connected, authenticated, and backed by a compiled driver.
5. ARC opens a CHUD snapshot by MAC and renders only writable, restorable controls that radio actually supports.
6. Review the capability-derived changes and explicitly select **Configure radio through CHUD**. Discovery and reads are automatic; writes are not.
7. Keep the radio connected while ARC executes and journals every required CHUD transaction. Reboot-required parameters are isolated into separate transactions.
8. Confirm when CHUD reports successful deferred readback. ARC performs a final snapshot comparison.
9. Only after every transaction passes does ARC display **Radio verified — safe to unplug**.
10. Choose **Add another radio** or **Finish swarm**.

If ARC reloads after a radio is verified, the wizard restores the most recently verified node rather than returning to an ambiguous discovery screen. The operator can continue with another radio or finish once the optional expected-count target is satisfied.

The first radio's supported desired values become suggestions for later radios. Unsupported keys are ignored rather than presented as universal capabilities. This matters because current CHUD mappings differ: Silvus exposes direct frequency and bandwidth controls, while TrellisWare RF selection is preset-based and does not expose direct management-IP, frequency, or bandwidth mappings.

## Identity and duplicate factory addresses

MAC is the workflow key; IP is only a current reach address. Two radios reporting the same factory IP remain separate records. If more than one manageable radio is connected, ARC requires the operator to select the intended physical radio instead of guessing.

## Recovery

ARC persists intent before crossing the CHUD side-effect boundary, including baseline values, desired values, per-transaction state, MAC, and any returned CHUD operation ID. After interruption:

- desired match means the transaction is verified without replay;
- baseline match permits an explicit retry;
- neither match is `diverged` and blocks automatic replay;
- an applying or awaiting-confirmation transaction becomes `recovery_required` after ARC restart.

CHUD retains completed operations for a limited period. If an operation is no longer retained, recovery relies on a fresh MAC-targeted snapshot rather than historical operation availability.

Some TrellisWare writes commit immediately. Rollback is therefore a best-effort compensating write and can fail after the management path changes. ARC never presents the workflow as one atomic fleet transaction.

## Completion

After every added radio passes configuration readback, ARC records the swarm as **configured—not topology verified**. It becomes topology validated only when CHUD supplies the expected live RF peer evidence. Current vendor telemetry gaps may leave this state outstanding even when configuration succeeded.

Deleting a draft, removing a definition, or archiving a swarm never resets or writes a physical radio.
ARC requires an explicit confirmation before deleting a swarm definition or removing a node definition. These confirmations state that the action affects only ARC's saved definition, receive keyboard focus, support Escape, and restore focus to the operator's prior control.

Server-side eligibility is authoritative even if a client bypasses the UI. Stage, deploy, reconcile, and confirm routes re-check current CHUD inventory and reject a MAC that is no longer confirmed or connected with an available management driver before crossing the hardware-write boundary.

## Local implementation evidence

The unpushed local implementation is isolated on `codex/42-complete-swarm-onboarding` in ARC, its ArcUI submodule, and AVIAN. Current automated evidence includes 382 passing dev-bridge library tests (with one ignored), 10 focused swarm API tests, and 26 focused ArcUI tests. Coverage includes strict MAC identity parsing, expected-size enforcement, reload recovery, the complete no-change onboarding path, keyboard modal behavior, destructive-action confirmation, and proof that definition deletion/removal never invokes the CHUD apply endpoint. The ArcUI production build, focused lint, and strict OpenSpec validation also pass. This evidence validates the software workflow only; it is not radio-in-the-loop or RF-topology validation.

## Validation still required

- Exercise the workflow against each supported vendor and deployed firmware.
- Test ordinary plus reboot-required changes during one physical connection.
- Disconnect and restart ARC at every transaction state.
- Confirm duplicate factory-IP radios never merge by address.
- Confirm final RF topology through CHUD where vendor APIs expose it.
