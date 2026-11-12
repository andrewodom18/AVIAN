import { createHash, randomUUID } from "node:crypto";
import { closeSync, existsSync, fsyncSync, openSync, readFileSync, renameSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import path from "node:path";

const MAX_STATE_BYTES = 32 * 1024 * 1024;
const digest = (state) => createHash("sha256").update(JSON.stringify(state)).digest("hex");

// The caller supplies a disposable directory. Never discover existing CHUD state.
export function stateStore(filename) {
  const file = path.resolve(filename);
  return {
    load() {
      if (!existsSync(file)) return null;
      if (statSync(file).size > MAX_STATE_BYTES) throw new Error("emulator state exceeds size limit");
      const envelope = JSON.parse(readFileSync(file, "utf8"));
      if (envelope.version !== 1 || envelope.digest !== digest(envelope.state)) {
        throw new Error("invalid emulator state envelope or checksum");
      }
      return envelope.state;
    },
    save(state) {
      const temporary = `${file}.${randomUUID()}.tmp`;
      let fd;
      try {
        const content = JSON.stringify({ version: 1, digest: digest(state), state });
        if (Buffer.byteLength(content) > MAX_STATE_BYTES) throw new Error("emulator state exceeds size limit");
        fd = openSync(temporary, "wx", 0o600);
        writeFileSync(fd, content);
        fsyncSync(fd);
        closeSync(fd);
        fd = undefined;
        renameSync(temporary, file);
      } finally {
        if (fd !== undefined) closeSync(fd);
        if (existsSync(temporary)) unlinkSync(temporary);
      }
    },
  };
}
