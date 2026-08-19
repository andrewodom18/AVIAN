# Signed RTL provisioning and SITL procedure

The only executable MAVLink action in this milestone is
`MAV_CMD_NAV_RETURN_TO_LAUNCH`, and `mesh-agent` permits execution only when
`commands.environment = "sitl"`. A hardware configuration using `execute` is
rejected at startup. Real Cube acceptance is dry-run only.

## Provision issuer keys

On the ground node:

```sh
sudo install -d -o avian -g avian -m 0700 /etc/avian/keys
sudo -u avian avianctl keys generate \
  --private-key /etc/avian/keys/ground-001.key \
  --public-key /etc/avian/keys/ground-001.pub
```

Keep the private key only on the ground node with mode `0600`. Transfer the
public key out of band to `/etc/avian/keys/ground-001.pub` on each authorized
aircraft. Configure the ground signing key and the aircraft issuer/public-key
pair as shown in the production examples. Node name and issuer ID must match.

Issuer nonce state, processed command IDs, receiver nonce state, pending
execution, and pending acknowledgements are atomically persisted under the
configured AVIAN storage directory. Preserve that state during upgrades.

## Real Cube dry-run acceptance

Use the production aircraft example with:

```toml
[commands]
mode = "dry_run"
environment = "hardware"
```

After the aircraft status shows the expected MAVLink system lock, issue from
the ground node:

```sh
avianctl emergency rtl --target aircraft-001
avianctl records --class acknowledgement
```

The acknowledgement must show `verified = true`, `accepted = true`, `executed
= false`, `command_mode = "dry_run"`, and the no-command detail. Use an
operator-approved MAVLink capture at the MAVProxy/Cube boundary to prove there
was no outgoing RTL `COMMAND_LONG`. Do not infer this solely from vehicle mode.

## ArduPilot SITL execution

Run this only on an isolated development host with no route to a physical
flight controller.

1. Start ArduPilot SITL and publish MAVLink to AVIAN's dedicated port:

   ```sh
   sim_vehicle.py -v ArduCopter -f quad --out=127.0.0.1:14553
   ```

2. Run a local aircraft agent on a unique PEAT port with
   `mavlink.address = "udpin:127.0.0.1:14553"`, the ground public key, and:

   ```toml
   [commands]
   mode = "execute"
   environment = "sitl"
   state_file = "sitl-command-state.json"
   lifetime_ms = 5000
   poll_ms = 250
   ack_timeout_ms = 1500
   retries = 1
   ```

3. Run a local ground agent with the private signing key. Exchange the two
   stable endpoint IDs and configure them as peers.
4. In the SITL console, establish a safe airborne state (`GUIDED`, arm, and a
   short takeoff) using the normal ArduPilot test procedure.
5. Confirm `avianctl status --json --require-ready` reports the SITL system lock.
6. Issue `avianctl emergency rtl --target sitl-aircraft` on the ground node.
7. Confirm SITL changes to RTL and emits a correlated `COMMAND_ACK`. Inspect the
   durable acknowledgement and require `verified`, `accepted`, and `executed`
   to be true with the accepted MAVLink result.
8. Restart the aircraft agent and confirm the same command ID is not executed
   again. A command with a stale nonce, expired timestamp, bad signature, wrong
   target, unlisted issuer/action, or missing system lock must fail closed.

Record the ArduPilot version, AVIAN commit, UTC timestamps, command ID, ACK
result, and sanitized console output. Never repeat the execute-mode procedure
against a real Cube.
