"""A stand-in for Google Drive's API, for forkz's tests (127.0.0.1 only):
the token endpoint (refresh tokens), files by ID in folders (listing in
pages, metadata, downloads with ranges, exports of Google documents),
creating folders, renaming and moving (by parents), trashing, copying,
resumable uploads in pieces, dates, and links. It starts with two files of
one name and a Google spreadsheet, as Drive allows. Not a real
implementation: just what forkz uses.

Usage: python3 gdrive_mock.py PORT
"""

import json
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

FOLDER = "application/vnd.google-apps.folder"
SHEET = "application/vnd.google-apps.spreadsheet"
items = {"root": {"id": "root", "name": "My Drive", "mimeType": FOLDER, "parents": [], "trashed": False}}
counter = [0]
tokens = set()
sessions = {}
shared = set()


def new_id():
    counter[0] += 1
    return "id%d" % counter[0]


def add(name, mime, parent, data=b""):
    item = {"id": new_id(), "name": name, "mimeType": mime, "parents": [parent], "trashed": False,
            "data": data, "modifiedTime": "2026-11-04T10:20:30.000Z"}
    items[item["id"]] = item
    return item


# Two files of one name, and a Google spreadsheet, at the top.
add("dup.txt", "text/plain", "root", b"first")
add("dup.txt", "text/plain", "root", b"second")
add("Budget", SHEET, "root")


def public(item):
    out = {k: item[k] for k in ("id", "name", "mimeType") if k in item}
    if item["mimeType"] != FOLDER and not item["mimeType"].startswith("application/vnd.google-apps."):
        out["size"] = str(len(item.get("data", b"")))
    out["modifiedTime"] = item.get("modifiedTime", "2026-11-04T10:20:30.000Z")
    return out


def subtree(item_id):
    found = [item_id]
    for other in list(items.values()):
        if item_id in other["parents"]:
            found.extend(subtree(other["id"]))
    return found


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("gdrive-mock: " + (fmt % args) + "\n")

    def reply(self, status, body=None, raw=None, headers=()):
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else b"")
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream" if raw is not None else "application/json")
        self.send_header("Content-Length", str(len(data)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(data)

    def error(self, status, message, reason="notFound"):
        self.reply(status, {"error": {"code": status, "message": message, "errors": [{"reason": reason}]}})

    def body(self):
        return self.rfile.read(int(self.headers.get("Content-Length") or 0))

    def authorized(self):
        if self.headers.get("Authorization", "").removeprefix("Bearer ") in tokens:
            return True
        self.error(401, "Invalid Credentials", "authError")
        return False

    def route(self, method):
        url = urlparse(self.path)
        query = {k: v[0] for k, v in parse_qs(url.query).items()}
        raw = self.body()
        if url.path == "/token" and method == "POST":
            form = {k: v[0] for k, v in parse_qs(raw.decode()).items()}
            if form.get("refresh_token") != "refresh-gd" or form.get("client_secret") != "test-secret":
                return self.reply(400, {"error": "invalid_grant", "error_description": "Bad Request"})
            token = "access-%d" % (len(tokens) + 1)
            tokens.add(token)
            return self.reply(200, {"access_token": token, "expires_in": 3599, "token_type": "Bearer"})
        if url.path.startswith("/upload-session/") and method == "PUT":
            return self.piece(url.path.rsplit("/", 1)[1], raw)
        if not self.authorized():
            return
        if url.path == "/drive/v3/about":
            return self.reply(200, {"user": {"emailAddress": "tester@example.com", "permissionId": "perm-1"}})
        upload = url.path.startswith("/upload/drive/v3/files")
        if upload:
            return self.start_upload(method, url.path, raw)
        m = re.fullmatch(r"/drive/v3/files(?:/(\w+))?(/export|/copy|/permissions)?", url.path)
        if not m:
            return self.error(404, "Not Found")
        item_id, action = m.group(1), m.group(2)
        if item_id is None and method == "GET":
            return self.list(query)
        if item_id is None and method == "POST":
            body = json.loads(raw)
            made = add(body["name"], body.get("mimeType", "application/octet-stream"), body["parents"][0])
            return self.reply(200, {"id": made["id"]})
        item = items.get(item_id)
        if not item or item["trashed"]:
            return self.error(404, "File not found: %s." % item_id)
        if action == "/export":
            return self.reply(200, raw=("exported " + item["name"] + " as " + query["mimeType"]).encode())
        if action == "/copy":
            body = json.loads(raw)
            made = add(body.get("name", item["name"]), item["mimeType"], body["parents"][0], item.get("data", b""))
            return self.reply(200, {"id": made["id"]})
        if action == "/permissions":
            shared.add(item_id)
            return self.reply(200, {"id": "anyoneWithLink"})
        if method == "GET" and query.get("alt") == "media":
            data = item.get("data", b"")
            ranged = self.headers.get("Range", "")
            if ranged.startswith("bytes="):
                return self.reply(206, raw=data[int(ranged[6:].split("-")[0]):])
            return self.reply(200, raw=data)
        if method == "GET":
            out = public(item)
            if "webViewLink" in query.get("fields", ""):
                out = {"webViewLink": "https://drive.google.com/file/d/%s/view" % item_id}
            return self.reply(200, out)
        if method == "PATCH":
            body = json.loads(raw) if raw else {}
            if "addParents" in query:
                item["parents"] = [query["addParents"]]
            if "name" in body:
                item["name"] = body["name"]
            if "modifiedTime" in body:
                item["modifiedTime"] = body["modifiedTime"]
            if body.get("trashed"):
                for gone in subtree(item_id):
                    items[gone]["trashed"] = True
            return self.reply(200, {"id": item_id})
        self.error(404, "Not Found")

    def list(self, query):
        m = re.match(r"'(\w+)' in parents", query.get("q", ""))
        folder = m.group(1)
        found = [public(i) for i in items.values() if folder in i["parents"] and not i["trashed"]]
        found.sort(key=lambda i: i["name"])
        start = int(query.get("pageToken", "0"))
        # Pages of three, to test paging.
        page = {"files": found[start:start + 3]}
        if start + 3 < len(found):
            page["nextPageToken"] = str(start + 3)
        self.reply(200, page)

    def start_upload(self, method, path, raw):
        body = json.loads(raw) if raw else {}
        sid = new_id()
        m = re.fullmatch(r"/upload/drive/v3/files/(\w+)", path)
        sessions[sid] = {"existing": m.group(1) if m else None, "meta": body, "data": b"",
                         "total": int(self.headers.get("X-Upload-Content-Length", "0"))}
        port = self.server.server_address[1]
        self.reply(200, {}, headers=[("Location", "http://127.0.0.1:%d/upload-session/%s" % (port, sid))])

    def piece(self, sid, raw):
        session = sessions.get(sid)
        if session is None:
            return self.error(404, "No such upload")
        content_range = self.headers.get("Content-Range", "")
        m = re.fullmatch(r"bytes (\d+)-(\d+)/(\d+)", content_range)
        if m and int(m.group(1)) != len(session["data"]):
            return self.error(400, "Bad offset", "badRequest")
        session["data"] += raw
        if len(session["data"]) < session["total"]:
            return self.reply(308, None, headers=[("Range", "bytes=0-%d" % (len(session["data"]) - 1))])
        if session["existing"]:
            item = items[session["existing"]]
            item["data"] = session["data"]
        else:
            meta = session["meta"]
            item = add(meta["name"], "application/octet-stream", meta["parents"][0], session["data"])
        del sessions[sid]
        self.reply(200, public(item))

    def do_GET(self):
        self.route("GET")

    def do_POST(self):
        self.route("POST")

    def do_PATCH(self):
        self.route("PATCH")

    def do_PUT(self):
        self.route("PUT")


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
