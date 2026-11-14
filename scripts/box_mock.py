"""A stand-in for Box's API, for forkz's live tests (scripts/test_protocols.sh).

It keeps everything in memory and is strict where Box is: refresh tokens
work once (each renewal gives a new one), access tokens are revoked now
and then (forkz must renew and retry), folders list in small pages
(markers), downloads redirect to another address, uploads in parts need
exact part sizes and SHA-1 digests, and a commit may answer 202 first.

    python3 scripts/box_mock.py PORT
"""

import base64
import hashlib
import itertools
import json
import re
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

CLIENT = ("forkz-test", "test-secret")
PAGE = 3
PART_SIZE = 1_000_000
REVOKE_EVERY = 25

lock = threading.Lock()
ids = itertools.count(100)
items = {"0": {"type": "folder", "id": "0", "name": "All Files", "parent": None}}
refresh_tokens = {"rt-0"}
access_tokens = set()
sessions = {}
api_requests = 0


def new_id():
    return str(next(ids))


def public(item):
    out = {k: item[k] for k in ("type", "id", "name")}
    out["size"] = len(item.get("data", b"")) if item["type"] == "file" else 0
    out["modified_at"] = "2026-11-04T02:20:30-08:00"
    if item["type"] == "file":
        out["content_modified_at"] = "2026-11-04T10:20:30Z"
    return out


def children(folder_id):
    return sorted((i for i in items.values() if i["parent"] == folder_id), key=lambda i: i["name"])


def conflict(parent, name, skip=None):
    for item in children(parent):
        if item["name"].lower() == name.lower() and item["id"] != skip:
            return item
    return None


def subtree(item_id):
    found = [item_id]
    for child in [i["id"] for i in items.values() if i["parent"] == item_id]:
        found.extend(subtree(child))
    return found


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.command, self.path))

    def reply(self, status, body=None, headers=()):
        data = b"" if body is None else json.dumps(body).encode()
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        if body is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def error(self, status, code, message, extra=None):
        body = {"type": "error", "status": status, "code": code, "message": message}
        if extra:
            body.update(extra)
        self.reply(status, body)

    def body(self):
        # Read once, before any answer (an answer that left a body unread
        # would garble the next request on the same connection).
        if not hasattr(self, "_body"):
            length = int(self.headers.get("Content-Length") or 0)
            self._body = self.rfile.read(length) if length else b""
        return self._body

    def authorized(self, api):
        global api_requests
        token = (self.headers.get("Authorization") or "").removeprefix("Bearer ")
        if api:
            api_requests += 1
            # Now and then the token stops working, as when it expires.
            if api_requests % REVOKE_EVERY == 0:
                access_tokens.discard(token)
        if token not in access_tokens:
            self.reply(401, None, [("WWW-Authenticate", 'Bearer error="invalid_token"')])
            return False
        return True

    def do_POST(self):
        with lock:
            self.route("POST")

    def do_GET(self):
        with lock:
            self.route("GET")

    def do_PUT(self):
        with lock:
            self.route("PUT")

    def do_DELETE(self):
        with lock:
            self.route("DELETE")

    def route(self, method):
        if hasattr(self, "_body"):
            del self._body
        self.body()
        url = urlparse(self.path)
        path, query = url.path, parse_qs(url.query)
        if path == "/oauth2/token" and method == "POST":
            return self.token()
        if path.startswith("/dl/") and method == "GET":
            return self.download(path[4:])
        upload = path.startswith("/api/2.0/")
        if not (upload or path.startswith("/2.0/")):
            return self.error(404, "not_found", "Not Found")
        if not self.authorized(api=not upload):
            return
        rest = path.split("/2.0/", 1)[1]
        if upload:
            return self.upload_route(method, rest)
        if rest == "users/me":
            return self.reply(200, {"type": "user", "id": "7", "login": "tester@example.com"})
        m = re.fullmatch(r"folders/(\w+)/items", rest)
        if m and method == "GET":
            return self.list(m.group(1), query)
        if rest == "folders" and method == "POST":
            return self.create_folder(json.loads(self.body()))
        m = re.fullmatch(r"files/(\w+)/content", rest)
        if m and method == "GET":
            if m.group(1) not in items:
                return self.error(404, "not_found", "Not Found")
            port = self.server.server_address[1]
            return self.reply(302, None, [("Location", f"http://127.0.0.1:{port}/dl/{m.group(1)}?signed=1")])
        m = re.fullmatch(r"files/(\w+)/copy", rest)
        if m and method == "POST":
            return self.copy(m.group(1), json.loads(self.body()))
        m = re.fullmatch(r"(files|folders)/(\w+)", rest)
        if m:
            kind, item_id = m.group(1)[:-1], m.group(2)
            item = items.get(item_id)
            if not item or item["type"] != kind:
                return self.error(404, "not_found", "Not Found")
            if method == "GET":
                return self.reply(200, public(item))
            if method == "PUT":
                return self.update(item, json.loads(self.body()))
            if method == "DELETE":
                if kind == "folder" and children(item_id) and query.get("recursive") != ["true"]:
                    return self.error(400, "folder_not_empty", "Folder is not empty")
                for gone in subtree(item_id):
                    items.pop(gone, None)
                return self.reply(204)
        self.error(404, "not_found", "Not Found")

    def token(self):
        form = {k: v[0] for k, v in parse_qs(self.body().decode()).items()}
        if (form.get("client_id"), form.get("client_secret")) != CLIENT:
            return self.reply(401, {"error": "invalid_client", "error_description": "The client credentials are invalid"})
        if form.get("grant_type") != "refresh_token" or form.get("refresh_token") not in refresh_tokens:
            return self.reply(400, {"error": "invalid_grant", "error_description": "Invalid refresh token"})
        # A refresh token works once.
        refresh_tokens.discard(form["refresh_token"])
        number = new_id()
        refresh, access = f"rt-{number}", f"at-{number}"
        refresh_tokens.add(refresh)
        access_tokens.add(access)
        self.reply(200, {"access_token": access, "refresh_token": refresh, "expires_in": 3600, "token_type": "bearer"})

    def list(self, folder_id, query):
        folder = items.get(folder_id)
        if not folder or folder["type"] != "folder":
            return self.error(404, "not_found", "Not Found")
        if query.get("usemarker") != ["true"]:
            return self.error(400, "bad_request", "use markers")
        start = int(query.get("marker", ["0"])[0] or 0)
        page = children(folder_id)[start:start + min(PAGE, int(query.get("limit", ["100"])[0]))]
        end = start + len(page)
        more = end < len(children(folder_id))
        self.reply(200, {"entries": [public(i) for i in page], "limit": PAGE, "next_marker": str(end) if more else ""})

    def create_folder(self, body):
        parent, name = body["parent"]["id"], body["name"]
        if parent not in items:
            return self.error(404, "not_found", "Not Found")
        if conflict(parent, name):
            return self.error(409, "item_name_in_use", "Item with the same name already exists")
        item = {"type": "folder", "id": new_id(), "name": name, "parent": parent}
        items[item["id"]] = item
        self.reply(201, public(item))

    def update(self, item, body):
        name = body.get("name", item["name"])
        parent = body.get("parent", {}).get("id", item["parent"])
        if conflict(parent, name, skip=item["id"]):
            return self.error(409, "item_name_in_use", "Item with the same name already exists")
        item["name"], item["parent"] = name, parent
        self.reply(200, public(item))

    def copy(self, item_id, body):
        item = items.get(item_id)
        if not item or item["type"] != "file":
            return self.error(404, "not_found", "Not Found")
        parent = body["parent"]["id"]
        name = body.get("name", item["name"])
        # Box does not copy over an existing item.
        if conflict(parent, name):
            return self.error(409, "item_name_in_use", "Item with the same name already exists")
        copied = {"type": "file", "id": new_id(), "name": name, "parent": parent, "data": item["data"]}
        items[copied["id"]] = copied
        self.reply(201, public(copied))

    def download(self, item_id):
        item = items.get(item_id)
        if not item or item["type"] != "file":
            return self.reply(404)
        data = item["data"]
        m = re.fullmatch(r"bytes=(\d+)-", self.headers.get("Range") or "")
        status, headers = 200, []
        if m:
            start = int(m.group(1))
            headers.append(("Content-Range", f"bytes {start}-{len(data) - 1}/{len(data)}"))
            data, status = data[start:], 206
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def upload_route(self, method, rest):
        if rest == "files/content" and method == "POST":
            return self.upload(None)
        m = re.fullmatch(r"files/(\w+)/content", rest)
        if m and method == "POST":
            return self.upload(m.group(1))
        if rest == "files/upload_sessions" and method == "POST":
            body = json.loads(self.body())
            if conflict(body["folder_id"], body["file_name"]):
                return self.error(409, "item_name_in_use", "Item with the same name already exists")
            return self.start_session(body["file_size"], body["folder_id"], body["file_name"], None)
        m = re.fullmatch(r"files/(\w+)/upload_sessions", rest)
        if m and method == "POST":
            body = json.loads(self.body())
            return self.start_session(body["file_size"], None, None, m.group(1))
        m = re.fullmatch(r"files/upload_sessions/(\w+)", rest)
        if m and method == "PUT":
            return self.part(sessions.get(m.group(1)))
        m = re.fullmatch(r"files/upload_sessions/(\w+)/commit", rest)
        if m and method == "POST":
            return self.commit(sessions.get(m.group(1)))
        self.error(404, "not_found", "Not Found")

    def upload(self, existing):
        boundary = re.search(r"boundary=(\S+)", self.headers.get("Content-Type", "")).group(1).encode()
        raw = self.body()
        parts = [p for p in raw.split(b"--" + boundary) if p not in (b"", b"--\r\n", b"\r\n")]
        fields = []
        for part in parts:
            head, _, content = part.lstrip(b"\r\n").partition(b"\r\n\r\n")
            name = re.search(rb'name="([^"]+)"', head).group(1).decode()
            fields.append((name, content[:-2] if content.endswith(b"\r\n") else content))
        # Box wants the attributes before the file.
        if [n for n, _ in fields] != ["attributes", "file"]:
            return self.error(400, "bad_request", "attributes must come first")
        attributes = json.loads(fields[0][1])
        data = fields[1][1]
        if existing:
            item = items.get(existing)
            if not item:
                return self.error(404, "not_found", "Not Found")
            item["data"] = data
        else:
            parent, name = attributes["parent"]["id"], attributes["name"]
            clash = conflict(parent, name)
            if clash:
                return self.error(409, "item_name_in_use", "Item with the same name already exists",
                                  {"context_info": {"conflicts": public(clash)}})
            item = {"type": "file", "id": new_id(), "name": name, "parent": parent, "data": data}
            items[item["id"]] = item
        self.reply(201, {"total_count": 1, "entries": [public(item)]})

    def start_session(self, size, folder, name, existing):
        session_id = "s" + new_id()
        sessions[session_id] = {"size": size, "folder": folder, "name": name, "existing": existing,
                                "parts": {}, "commits": 0}
        total = (size + PART_SIZE - 1) // PART_SIZE
        self.reply(201, {"id": session_id, "part_size": PART_SIZE, "total_parts": total})

    def part(self, session):
        if not session:
            return self.error(404, "not_found", "Not Found")
        data = self.body()
        m = re.fullmatch(r"bytes (\d+)-(\d+)/(\d+)", self.headers.get("Content-Range", ""))
        start, end, total = (int(x) for x in m.groups())
        if total != session["size"] or end - start + 1 != len(data) or start % PART_SIZE:
            return self.error(416, "range_not_satisfiable", "Bad range")
        if len(data) != PART_SIZE and end != total - 1:
            return self.error(416, "range_not_satisfiable", "Only the last part may be smaller")
        digest = base64.b64encode(hashlib.sha1(data).digest()).decode()
        if self.headers.get("Digest") != f"sha={digest}":
            return self.error(412, "precondition_failed", "Digest mismatch")
        part = {"part_id": f"p{start}", "offset": start, "size": len(data), "sha1": hashlib.sha1(data).hexdigest()}
        session["parts"][start] = (part, data)
        self.reply(200, {"part": part})

    def commit(self, session):
        if not session:
            return self.error(404, "not_found", "Not Found")
        body = json.loads(self.body())
        offsets = sorted(session["parts"])
        if [p["offset"] for p in body["parts"]] != offsets:
            return self.error(400, "bad_request", "Parts do not match")
        data = b"".join(session["parts"][o][1] for o in offsets)
        digest = base64.b64encode(hashlib.sha1(data).digest()).decode()
        if len(data) != session["size"] or self.headers.get("Digest") != f"sha={digest}":
            return self.error(412, "precondition_failed", "Digest or size mismatch")
        # The first commit is still being processed.
        session["commits"] += 1
        if session["commits"] == 1:
            return self.reply(202, None, [("Retry-After", "1")])
        if session["existing"]:
            item = items[session["existing"]]
            item["data"] = data
        else:
            item = {"type": "file", "id": new_id(), "name": session["name"], "parent": session["folder"], "data": data}
            items[item["id"]] = item
        self.reply(201, {"total_count": 1, "entries": [public(item)]})


if __name__ == "__main__":
    port = int(sys.argv[1])
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
