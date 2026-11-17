# Linux packaging

Build the scoped helper and patched engine:

```sh
python3 qualification/build.py --managed-network --release --output /tmp/avian-engine
```

The manager and engine run under an explicitly selected existing account. The helper runs as root with `CAP_NET_ADMIN` and `CAP_NET_RAW`, receives its protected socket from systemd, and reads a root-owned JSON configuration. Its `allowed_uid` must match the manager account. Its fixed runtime directory is `/run/avian/INSTANCE`.

A helper restart retains the existing TUN, routes, and borrowed sockets when its root-owned journal matches the same live manager UID/PID, kernel boot/start identity, helper namespaces, and owned interface scope. Dead or mismatched owners and legacy ownerless journals trigger cleanup of the verified owned resources. Interface identity or foreign-resource conflicts are refused.

Procfs must expose the manager's `/proc/PID/stat` and `/proc/PID/status`, the kernel boot ID, and the helper's own namespace descriptors. Owner verification uses the existing network capabilities; it does not add tracing privileges.

Render account-specific units **before** building an archive and collecting qualification evidence:

```sh
python3 packaging/linux/render.py --user rownd --instance mesh \
  --helper-config /path/to/network.json --output /tmp/avian-units
```

The JSON helper configuration pins the node ID, TUN name, entire RFC1918 overlay prefix, host address, approved physical interfaces, listener port, and MTU. Set the helper MTU to `1320`; the engine configuration uses `1380` before encryption and forwarding overhead.

The manager runtime configuration requires:

```toml
[runtime]
execution = "native"
isolation_required = false
network_helper_socket = "/run/avian/mesh/network.sock"
interface_name = "avian0"
overlay_prefix_length = 24
measured_routing = true
private_rpc = true
underlay_binding = true
priority_forwarding = true
```

Identity keys, transport credentials, API tokens, membership files, and the runtime directory must belong to the selected account. Private files use mode `0600` and private directories use `0700`. Keep the helper configuration and its journal directory root-owned. Socket activation assigns the socket to the selected account without granting the helper ownership-changing capabilities.

`install.py` stages only a package matching an explicit approved SHA256 and passing native core evidence. It enables no services. Release publication additionally requires all platform gates and physical ARC/Tank and CHUD evidence; isolated container checks do not substitute for those gates.
