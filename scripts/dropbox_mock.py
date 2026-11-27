"""A stand-in for Dropbox's API, for forkz's tests (127.0.0.1 only): the
token endpoint (refresh tokens), the file calls forkz makes (list, get
metadata, create folder, move, copy, delete, upload, upload sessions,
download with ranges), and shared links (expiry refused, as on a basic
account). Not a real implementation: just what forkz uses.

Usage: python3 dropbox_mock.py PORT
"""

import hashlib
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

# Lowercase path -> {"name", "path" (as written), "folder", "data"}.
items = {"": {"name": "", "path": "", "folder": True}}
sessions = {}
tokens = {"access-0"}
counter = [0]


def key(path):
    return path.rstrip("/").lower()


def parent(path):
    return key(path).rsplit("/", 1)[0] if "/" in key(path) else ""


def metadata(item):
    out = {".tag": "folder" if item["folder"] else "file", "name": item["name"],
           "path_display": item["path"], "path_lower": key(item["path"]), "id": "id:" + key(item["path"])}
    if not item["folder"]:
        out["size"] = len(item["data"])
        out["client_modified"] = "2026-11-04T10:20:30Z"
        out["server_modified"] = "2026-11-04T10:20:31Z"
        out["content_hash"] = content_hash(item["data"])
    return out


def content_hash(data):
    """Dropbox's content hash: SHA-256 of the SHA-256 of each 4 MiB block."""
    block = 4 * 1024 * 1024
    digests = b"".join(hashlib.sha256(data[i:i + block]).digest() for i in range(0, len(data), block))
    return hashlib.sha256(digests).hexdigest()


def children(path):
    k = key(path)
    return [i for p, i in sorted(items.items()) if p and parent(p) == k]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("dropbox-mock: " + (fmt % args) + "\n")

    def reply(self, status, body=None, raw=None, headers=()):
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else b"")
        self.send_response(status)
        self.send_header("Content-Type", "application/octet-stream" if raw is not None else "application/json")
        self.send_header("Content-Length", str(len(data)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(data)

    def error(self, summary):
        self.reply(409, {"error_summary": summary + "/..", "error": {".tag": summary.split("/")[0]}})

    def body(self):
        return self.rfile.read(int(self.headers.get("Content-Length") or 0))

    def do_POST(self):
        url = urlparse(self.path)
        raw = self.body()
        if url.path == "/oauth2/token":
            form = {k: v[0] for k, v in parse_qs(raw.decode()).items()}
            if form.get("client_id") != "forkz-test" or form.get("refresh_token") != "refresh-dbx":
                return self.reply(400, {"error": "invalid_grant", "error_description": "refresh token is invalid"})
            counter[0] += 1
            token = "access-%d" % counter[0]
            tokens.add(token)
            return self.reply(200, {"access_token": token, "token_type": "bearer", "expires_in": 14400})
        if self.headers.get("Authorization", "").removeprefix("Bearer ") not in tokens:
            return self.reply(401, {"error_summary": "invalid_access_token/"})
        route = url.path.removeprefix("/2/")
        arg = json.loads(self.headers.get("Dropbox-API-Arg") or "null")
        args = arg if arg is not None else (json.loads(raw) if raw and raw != b"null" else {})
        handler = getattr(self, "r_" + route.replace("/", "_"), None)
        if handler is None:
            return self.reply(404, {"error_summary": "unknown route " + route})
        return handler(args, raw)

    def r_users_get_current_account(self, args, raw):
        self.reply(200, {"account_id": "dbid:test", "email": "tester@example.com"})

    def r_files_list_folder(self, args, raw):
        if key(args["path"]) not in items or not items[key(args["path"])]["folder"]:
            return self.error("path/not_found")
        everything = [metadata(i) for i in children(args["path"])]
        # Pages of three, to test paging.
        sessions["cursor"] = everything[3:]
        self.reply(200, {"entries": everything[:3], "cursor": "cursor", "has_more": len(everything) > 3})

    def r_files_list_folder_continue(self, args, raw):
        rest = sessions.get(args["cursor"], [])
        sessions[args["cursor"]] = rest[3:]
        self.reply(200, {"entries": rest[:3], "cursor": args["cursor"], "has_more": len(rest) > 3})

    def r_files_get_metadata(self, args, raw):
        item = items.get(key(args["path"]))
        if not item:
            return self.error("path/not_found")
        self.reply(200, metadata(item))

    def r_files_create_folder_v2(self, args, raw):
        path = args["path"]
        if key(path) in items:
            return self.error("path/conflict/folder")
        if parent(path) not in items:
            return self.error("path/not_found")
        items[key(path)] = {"name": path.rsplit("/", 1)[1], "path": path, "folder": True}
        self.reply(200, {"metadata": metadata(items[key(path)])})

    def subtree(self, path):
        k = key(path)
        return [p for p in items if p == k or p.startswith(k + "/")]

    def r_files_move_v2(self, args, raw, copy=False):
        source, target = args["from_path"], args["to_path"]
        if key(source) not in items:
            return self.error("from_lookup/not_found")
        if key(target) in items and key(target) != key(source):
            return self.error("to/conflict/file")
        moved = {}
        for p in self.subtree(source):
            item = dict(items[p])
            new_path = target + item["path"][len(source):]
            item["path"] = new_path
            item["name"] = new_path.rsplit("/", 1)[1]
            moved[key(new_path)] = item
        if not copy:
            for p in self.subtree(source):
                del items[p]
        items.update(moved)
        self.reply(200, {"metadata": metadata(items[key(target)])})

    def r_files_copy_v2(self, args, raw):
        self.r_files_move_v2(args, raw, copy=True)

    def r_files_delete_v2(self, args, raw):
        if key(args["path"]) not in items:
            return self.error("path_lookup/not_found")
        gone = metadata(items[key(args["path"])])
        for p in self.subtree(args["path"]):
            del items[p]
        self.reply(200, {"metadata": gone})

    def store(self, path, data):
        if parent(path) not in items:
            return self.error("path/not_found")
        items[key(path)] = {"name": path.rsplit("/", 1)[1], "path": path, "folder": False, "data": data}
        self.reply(200, metadata(items[key(path)]))

    def r_files_upload(self, args, raw):
        self.store(args["path"], raw)

    def r_files_upload_session_start(self, args, raw):
        counter[0] += 1
        sid = "session-%d" % counter[0]
        sessions[sid] = raw
        self.reply(200, {"session_id": sid})

    def r_files_upload_session_append_v2(self, args, raw):
        cursor = args["cursor"]
        if len(sessions[cursor["session_id"]]) != cursor["offset"]:
            return self.error("incorrect_offset")
        sessions[cursor["session_id"]] += raw
        self.reply(200, None)

    def r_files_upload_session_finish(self, args, raw):
        cursor = args["cursor"]
        data = sessions.pop(cursor["session_id"]) + raw
        self.store(args["commit"]["path"], data)

    def r_files_download(self, args, raw):
        item = items.get(key(args["path"]))
        if not item or item["folder"]:
            return self.error("path/not_found")
        data = item["data"]
        ranged = self.headers.get("Range", "")
        if ranged.startswith("bytes="):
            start = int(ranged[6:].split("-")[0])
            return self.reply(206, raw=data[start:])
        self.reply(200, raw=data, headers=[("Dropbox-API-Result", json.dumps(metadata(item)))])

    def r_sharing_create_shared_link_with_settings(self, args, raw):
        if key(args["path"]) not in items:
            return self.error("path/not_found")
        if "settings" in args and "expires" in args.get("settings", {}):
            return self.error("settings_error/not_authorized")
        self.reply(200, {"url": "https://www.dropbox.com/s/test/" + items[key(args["path"])]["name"]})

    def r_sharing_list_shared_links(self, args, raw):
        self.reply(200, {"links": []})


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
