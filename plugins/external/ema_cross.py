#!/usr/bin/env python3
"""An EMA cross strategy for Persephone, running as a process of its own.

Persephone talks to it over a socket: JSON, one object per line each way, one answer per
request, so backtests run in lockstep. Anything Python can do (pandas, scikit-learn, PyTorch,
an agent framework) can go in `on_bar`.

    python3 ema_cross.py [PORT]        # listens on 127.0.0.1:7878 by default

Then copy `ema_cross.external` (it holds the address) into Persephone's plugins folder
(`persephone-cli plugin dir` prints it). "EMA Cross (Python)" shows up with the strategies.
Or backtest it headless: `persephone-cli backtest --external tcp://127.0.0.1:7878`.

Requests and answers:
    {"type": "describe"}                -> {"name", "description", "params": [...]}
    {"type": "start", "params": [...]}  -> {}
    {"type": "bar", "ts", "open", "high", "low", "close", "volume"}
                                        -> {"target": 1 | -1 | 0 | null}
    {"type": "end"}                     -> {"notes": [{"text", "warning"}]}
Any answer can be {"error": "..."} instead; the run stops with that message.
"""

import json
import socketserver
import sys

DESCRIPTION = {
    "name": "EMA Cross (Python)",
    "description": "Long when the fast EMA crosses above the slow one, short below.",
    "params": [
        {"name": "Fast", "default": 10, "min": 1, "max": 500, "integer": True},
        {"name": "Slow", "default": 30, "min": 2, "max": 1000, "integer": True},
    ],
}


class EmaCross:
    def __init__(self, fast, slow):
        if fast >= slow:
            raise ValueError("Fast must be shorter than Slow")
        self.alphas = (2 / (fast + 1), 2 / (slow + 1))
        self.emas = None
        self.fast_above = None
        self.bars = 0

    def on_bar(self, bar):
        """The position wanted after this bar: 1 long, -1 short, 0 flat, None to keep."""
        self.bars += 1
        close = bar["close"]
        if self.emas is None:
            self.emas = [close, close]
        else:
            self.emas = [ema + a * (close - ema) for ema, a in zip(self.emas, self.alphas)]
        fast_above = self.emas[0] > self.emas[1]
        crossed = self.fast_above is not None and fast_above != self.fast_above
        self.fast_above = fast_above
        if not crossed:
            return None
        return 1 if fast_above else -1


class Connection(socketserver.StreamRequestHandler):
    """One connection: a description, or one backtest or live session."""

    def handle(self):
        strategy = None
        for line in self.rfile:
            request = json.loads(line)
            try:
                kind = request["type"]
                if kind == "describe":
                    answer = DESCRIPTION
                elif kind == "start":
                    fast, slow = (int(p) for p in request["params"])
                    strategy = EmaCross(fast, slow)
                    answer = {}
                elif kind == "bar":
                    answer = {"target": strategy.on_bar(request)}
                elif kind == "end":
                    answer = {"notes": [{"text": f"Python saw {strategy.bars} bars", "warning": False}]}
                else:
                    answer = {"error": f"unknown request {kind!r}"}
            except Exception as error:  # the run stops with this message
                answer = {"error": str(error)}
            self.wfile.write((json.dumps(answer) + "\n").encode())
            self.wfile.flush()


class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 7878
    with Server(("127.0.0.1", port), Connection) as server:
        print(f"EMA Cross (Python) listening on 127.0.0.1:{port}", flush=True)
        server.serve_forever()
