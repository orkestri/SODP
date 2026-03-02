"""Collaborative text editor — demo backend.

Serves the frontend and issues short-lived SODP JWT tokens so the browser
can connect directly to the SODP WebSocket server.

Usage:
    pip install flask PyJWT
    # Terminal 1 — SODP server
    SODP_JWT_SECRET=demo-secret ./target/debug/sodp-server 0.0.0.0:7777
    # Terminal 2 — this backend
    python demo-collab/backend.py
    # Then open http://localhost:8080 in multiple browser tabs.
"""

import os, time, uuid
import jwt
from flask import Flask, jsonify, request, send_from_directory

JWT_SECRET = "demo-secret"   # must match SODP_JWT_SECRET env var
JWT_TTL    = 3600            # 1-hour tokens
PORT       = 8084
STATIC     = os.path.join(os.path.dirname(__file__), "static")

app = Flask(__name__)


@app.route("/token")
def get_token():
    """Issue a JWT for the given display name.  Called by the browser on join."""
    name = (request.args.get("name") or "Guest").strip()[:40] or "Guest"
    sub  = uuid.uuid4().hex[:8]
    tok  = jwt.encode(
        {"sub": sub, "name": name, "exp": int(time.time()) + JWT_TTL},
        JWT_SECRET,
        algorithm="HS256",
    )
    return jsonify({"token": tok, "sub": sub, "name": name})


@app.route("/")
def index():
    return send_from_directory(STATIC, "index.html")


if __name__ == "__main__":
    print(f"Collab editor: http://localhost:{PORT}")
    print(f"SODP server needed: SODP_JWT_SECRET={JWT_SECRET} ./target/debug/sodp-server 0.0.0.0:7777")
    app.run(host="0.0.0.0", port=PORT, threaded=True, debug=False)
