"""Minimal MCP stdio istemcisi — `rag --mcp-server` sürecini sürer.

Satır-ayraçlı JSON-RPC 2.0 (sunucu src/mcp/mod.rs satırları okur/yazar).
Akış: initialize -> notifications/initialized -> tools/call ...
"""
import json
import subprocess
from pathlib import Path

# Sunucu .rag/ ve modeli proje kökünde arar → cwd'yi köke sabitle.
ROOT = Path(__file__).resolve().parent.parent


class RagMcp:
    def __init__(self, cmd=("rag", "--mcp-server")):
        self.proc = subprocess.Popen(
            list(cmd),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            text=True, bufsize=1, cwd=str(ROOT),
        )
        self._id = 0
        self._initialize()

    def _send(self, obj):
        self.proc.stdin.write(json.dumps(obj) + "\n")
        self.proc.stdin.flush()

    def _read_response(self, expected_id):
        # id'si eşleşen ilk yanıtı bekle (notification'ları atla)
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise RuntimeError("MCP sunucusu beklenmedik şekilde kapandı")
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            if msg.get("id") == expected_id:
                return msg

    def _rpc(self, method, params=None):
        self._id += 1
        rid = self._id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method,
                    "params": params or {}})
        return self._read_response(rid)

    def _initialize(self):
        self._rpc("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "bench", "version": "0"},
        })
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call_tool(self, name, arguments):
        """tool_text içeriğini (LLM bağlamına giren metin) döndürür."""
        resp = self._rpc("tools/call", {"name": name, "arguments": arguments})
        if "error" in resp:
            raise RuntimeError(f"{name} hata: {resp['error']}")
        content = resp.get("result", {}).get("content", [])
        return "".join(c.get("text", "") for c in content if c.get("type") == "text")

    def close(self):
        try:
            self.proc.stdin.close()
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()


if __name__ == "__main__":
    # Hızlı duman testi
    m = RagMcp()
    try:
        out = m.call_tool("context.bundle",
                          {"task": "ensure_index nasil calisir", "budget_tokens": 6000})
        print(f"context.bundle döndü, {len(out)} karakter")
    finally:
        m.close()
