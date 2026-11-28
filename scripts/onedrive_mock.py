"""A stand-in for Microsoft Graph's OneDrive API, for forkz's tests
(127.0.0.1 only): the token endpoint (refresh tokens, replaced each time),
the account and drive, items by path (get, children with paging, create a
folder, rename and move, set dates, delete), simple uploads and upload
sessions, pre-signed downloads with ranges, copies with a monitor page, and
share links (expiry refused, as on a personal account without Microsoft
365). Not a real implementation: just what forkz uses.

Usage: python3 onedrive_mock.py PORT
"""

import base64
import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, unquote, urlparse

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 9780
BASE = "http://127.0.0.1:%d" % PORT

# Lowercase path ("" is the top) -> {"id", "name", "folder", "data", "modified"}.
items = {"": {"id": "root", "name": "root", "folder": True, "modified": "2026-11-04T10:20:30Z"}}
sessions = {}
monitors = {}
tokens = set()
refresh = {"current": "refresh-od"}
counter = [0]


def next_id(prefix):
    counter[0] += 1
    return "%s-%d" % (prefix, counter[0])


def key(path):
    return path.strip("/").lower()


def parent(k):
    return k.rsplit("/", 1)[0] if "/" in k else ""


def by_id(item_id):
    for k, item in items.items():
        if item["id"] == item_id:
            return k
    return None


def quick_xor(data):
    """Microsoft's QuickXorHash: each byte XORed into a 160-bit value 11
    bits further along (wrapping around), then the length into the last 8
    bytes. Written as a rotate, not as forkz's port of the C#, so the two
    check each other."""
    value, mask = 0, (1 << 160) - 1
    for i, byte in enumerate(data):
        shifted = byte << ((i * 11) % 160)
        value ^= (shifted | (shifted >> 160)) & mask
    out = bytearray(value.to_bytes(20, "little"))
    for i, b in enumerate(len(data).to_bytes(8, "little")):
        out[12 + i] ^= b
    return base64.b64encode(bytes(out)).decode()


def public(k):
    item = items[k]
    out = {"id": item["id"], "name": item["name"], "lastModifiedDateTime": "2026-11-28T09:00:00Z",
           "fileSystemInfo": {"lastModifiedDateTime": item["modified"]}}
    if item["folder"]:
        out["folder"] = {"childCount": len(children(k))}
        out["size"] = 0
    else:
        out["file"] = {"hashes": {"quickXorHash": quick_xor(item["data"])}}
        out["size"] = len(item["data"])
        out["@microsoft.graph.downloadUrl"] = BASE + "/download/" + item["id"]
    return out


def children(k):
    return [p for p in sorted(items) if p and parent(p) == k]


def store(k, data):
    old = items.get(k)
    if old and old["folder"]:
        return None
    name = k.rsplit("/", 1)[-1]
    items[k] = {"id": old["id"] if old else next_id("file"), "name": old["name"] if old else name,
                "folder": False, "data": data, "modified": "2026-11-28T09:00:00Z"}
    return k


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("onedrive-mock: " + (fmt % args) + "\n")

    def reply(self, status, body=None, raw=None, headers=()):
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else b"")
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream" if raw is not None else "application/json")
        self.send_header("Content-Length", str(len(data)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(data)

    def error(self, status, code, message):
        self.reply(status, {"error": {"code": code, "message": message}})

    def body(self):
        return self.rfile.read(int(self.headers.get("Content-Length") or 0))

    def signed(self):
        """Graph wants the token; the pre-signed URLs refuse it."""
        return self.headers.get("Authorization", "").removeprefix("Bearer ") in tokens

    def route(self, method):
        url = urlparse(self.path)
        raw = self.body()
        query = {k: v[0] for k, v in parse_qs(url.query).items()}
        if url.path == "/token" and method == "POST":
            return self.token(raw)
        if url.path.startswith("/download/"):
            return self.download(url.path.removeprefix("/download/"))
        if url.path.startswith("/upload/"):
            return self.upload_part(url.path.removeprefix("/upload/"), raw)
        if url.path.startswith("/monitor/"):
            return self.reply(202 if monitors.get(url.path) == "new" else 200, self.monitor(url.path))
        if not self.signed():
            return self.error(401, "InvalidAuthenticationToken", "Access token is empty or invalid.")
        path = unquote(url.path)
        if path == "/v1.0/me":
            return self.reply(200, {"mail": "tester@example.com", "userPrincipalName": "tester@example.com"})
        if path == "/v1.0/me/drive":
            return self.reply(200, {"id": "drive-test", "driveType": "personal"})
        if path.startswith("/v1.0/me/drive/root"):
            rest = path.removeprefix("/v1.0/me/drive/root")
            if rest.startswith(":"):
                rest = rest[1:]
                target, _, action = rest.partition(":")
            else:
                target, action = "", rest
            return self.item(method, key(target), action, raw, query)
        self.error(404, "invalidRequest", "unknown route " + path)

    def token(self, raw):
        form = {k: v[0] for k, v in parse_qs(raw.decode()).items()}
        if form.get("client_id") != "forkz-test" or form.get("refresh_token") != refresh["current"]:
            return self.reply(400, {"error": "invalid_grant", "error_description": "AADSTS70000: The grant is invalid."})
        if "Files.ReadWrite.All" not in form.get("scope", ""):
            return self.reply(400, {"error": "invalid_scope"})
        access = next_id("access")
        tokens.add(access)
        # Microsoft replaces the refresh token each time.
        refresh["current"] = next_id("refresh")
        self.reply(200, {"access_token": access, "refresh_token": refresh["current"], "expires_in": 3600})

    def item(self, method, k, action, raw, query):
        if action == "/children" and method == "GET":
            if k not in items or not items[k]["folder"]:
                return self.error(404, "itemNotFound", "Item not found")
            everything = [public(c) for c in children(k)]
            # Pages of three, to test paging.
            start = int(query.get("skip", "0"))
            page = {"value": everything[start:start + 3]}
            if start + 3 < len(everything):
                page["@odata.nextLink"] = "%s%s?skip=%d" % (BASE, urlparse(self.path).path, start + 3)
            return self.reply(200, page)
        if action == "/children" and method == "POST":
            body = json.loads(raw)
            new = (k + "/" if k else "") + body["name"].lower()
            if new in items:
                return self.error(409, "nameAlreadyExists", "Name already exists")
            items[new] = {"id": next_id("folder"), "name": body["name"], "folder": True,
                          "modified": "2026-11-28T09:00:00Z"}
            return self.reply(201, public(new))
        if action == "/content" and method == "PUT":
            if store(k, raw) is None:
                return self.error(409, "nameAlreadyExists", "A folder is there")
            return self.reply(201, public(k))
        if action == "/createUploadSession" and method == "POST":
            sid = next_id("session")
            sessions[sid] = {"key": k, "data": b""}
            return self.reply(200, {"uploadUrl": BASE + "/upload/" + sid})
        if k not in items:
            return self.error(404, "itemNotFound", "Item not found")
        if action == "/copy" and method == "POST":
            body = json.loads(raw)
            into = by_id(body["parentReference"]["id"])
            new = (into + "/" if into else "") + body["name"].lower()
            for p in [k] + [c for c in list(items) if c.startswith(k + "/")]:
                # As OneDrive does: a copy is dated when it was made.
                copied = dict(items[p], id=next_id("copy"), modified="2026-11-28T09:30:00Z")
                if p == k:
                    copied["name"] = body["name"]
                items[new + p[len(k):]] = copied
            monitor = "/monitor/" + next_id("copy")
            monitors[monitor] = "new"
            return self.reply(202, None, headers=[("Location", BASE + monitor)])
        if action == "/createLink" and method == "POST":
            body = json.loads(raw)
            if "expirationDateTime" in body:
                return self.error(400, "invalidRequest", "Expiration is not supported for this account.")
            return self.reply(201, {"link": {"webUrl": "https://1drv.ms/u/s!test-" + items[k]["id"]}})
        if action == "" and method == "GET":
            item = public(k)
            # As Graph does: $select picks fields, and never includes the
            # download URL (only the whole item has it).
            if "$select" in query:
                wanted = query["$select"].split(",")
                item = {f: v for f, v in item.items() if f in wanted and not f.startswith("@")}
            return self.reply(200, item)
        if action == "" and method == "DELETE":
            for p in [c for c in list(items) if c == k or c.startswith(k + "/")]:
                del items[p]
            return self.reply(204)
        if action == "" and method == "PATCH":
            return self.patch(k, json.loads(raw))
        self.error(400, "invalidRequest", "unsupported " + method + " " + action)

    def patch(self, k, body):
        if "fileSystemInfo" in body:
            items[k]["modified"] = body["fileSystemInfo"]["lastModifiedDateTime"]
        if "name" in body or "parentReference" in body:
            into = by_id(body["parentReference"]["id"]) if "parentReference" in body else parent(k)
            name = body.get("name", items[k]["name"])
            new = (into + "/" if into else "") + name.lower()
            if new != k and new in items:
                return self.error(409, "nameAlreadyExists", "Name already exists")
            for p in sorted([c for c in list(items) if c == k or c.startswith(k + "/")]):
                items[new + p[len(k):]] = items.pop(p)
            items[new]["name"] = name
            k = new
        self.reply(200, public(k))

    def upload_part(self, sid, raw):
        if "Authorization" in self.headers:
            return self.error(401, "unauthenticated", "Do not send a token to the upload URL.")
        session = sessions.get(sid)
        if session is None:
            return self.error(404, "itemNotFound", "No such session")
        span, _, total = self.headers.get("Content-Range", "").removeprefix("bytes ").partition("/")
        start, _, end = span.partition("-")
        if int(start) != len(session["data"]) or int(end) - int(start) + 1 != len(raw):
            return self.error(416, "invalidRange", "Fragment overlap")
        if len(raw) % (320 * 1024) and int(end) + 1 != int(total):
            return self.error(400, "invalidRequest", "Parts must be multiples of 320 KiB")
        session["data"] += raw
        if len(session["data"]) < int(total):
            return self.reply(202, {"nextExpectedRanges": ["%d-" % len(session["data"])]})
        del sessions[sid]
        store(session["key"], session["data"])
        self.reply(201, public(session["key"]))

    def download(self, item_id):
        if "Authorization" in self.headers:
            return self.error(401, "unauthenticated", "Pre-signed URLs take no token.")
        k = by_id(item_id)
        if k is None or items[k]["folder"]:
            return self.error(404, "itemNotFound", "Item not found")
        data = items[k]["data"]
        ranged = self.headers.get("Range", "")
        if ranged.startswith("bytes="):
            return self.reply(206, raw=data[int(ranged[6:].split("-")[0]):])
        self.reply(200, raw=data)

    def monitor(self, path):
        # The first look: still copying; then done.
        if monitors.get(path) == "new":
            monitors[path] = "done"
            return {"status": "inProgress", "percentageComplete": 50}
        return {"status": "completed"}

    def do_GET(self):
        self.route("GET")

    def do_POST(self):
        self.route("POST")

    def do_PUT(self):
        self.route("PUT")

    def do_PATCH(self):
        self.route("PATCH")

    def do_DELETE(self):
        self.route("DELETE")


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
