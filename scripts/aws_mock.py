"""A stand-in for AWS's sign-in services, for forkz's tests (127.0.0.1
only): IAM Identity Center's device sign-in (OIDC), its portal (role keys
for a signed-in user), and STS AssumeRole. Not a real implementation:
just enough to check what forkz sends and does with the answers.

Usage: python3 aws_mock.py PORT
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

stats = {"registered": 0, "device": 0, "refreshed": 0, "assumed": 0, "pending": 0}
tokens = set()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("aws-mock: " + (fmt % args) + "\n")

    def reply(self, status, body, content_type="application/json"):
        data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def body(self):
        return self.rfile.read(int(self.headers.get("Content-Length") or 0))

    def do_GET(self):
        url = urlparse(self.path)
        if url.path == "/stats":
            return self.reply(200, stats)
        if url.path == "/federation/credentials":
            query = parse_qs(url.query)
            if self.headers.get("x-amz-sso_bearer_token") not in tokens:
                return self.reply(401, {"message": "Session token not found or invalid"})
            if query.get("account_id") != ["111122223333"] or query.get("role_name") != ["Dev"]:
                return self.reply(403, {"message": "No access"})
            return self.reply(200, {"roleCredentials": {
                "accessKeyId": "ASIASSOEXAMPLE",
                "secretAccessKey": "sso-secret",
                "sessionToken": "sso-session-token",
                "expiration": int((time.time() + 3600) * 1000),
            }})
        self.reply(404, {"message": "Not Found"})

    def do_POST(self):
        url = urlparse(self.path)
        raw = self.body()
        if url.path == "/client/register":
            stats["registered"] += 1
            return self.reply(200, {"clientId": "client-1", "clientSecret": "client-secret",
                                    "clientSecretExpiresAt": int(time.time()) + 90 * 86400})
        if url.path == "/device_authorization":
            stats["device"] += 1
            body = json.loads(raw)
            assert body["startUrl"] == "https://corp.awsapps.com/start", body
            return self.reply(200, {"deviceCode": "device-1", "userCode": "ABCD-EFGH",
                                    "verificationUri": "https://device.sso.example/",
                                    "verificationUriComplete": "https://device.sso.example/?user_code=ABCD-EFGH",
                                    "expiresIn": 600, "interval": 1})
        if url.path == "/token":
            body = json.loads(raw)
            if body.get("clientSecret") != "client-secret":
                return self.reply(400, {"error": "invalid_client"})
            if body["grantType"] == "refresh_token":
                if body.get("refreshToken") != "refresh-1":
                    return self.reply(400, {"error": "invalid_grant"})
                stats["refreshed"] += 1
                token = "access-%d" % (len(tokens) + 1)
                tokens.add(token)
                return self.reply(200, {"accessToken": token, "expiresIn": 3600, "refreshToken": "refresh-1"})
            # The device sign-in: pending once (the user is still in the
            # browser), then done.
            if stats["pending"] < stats["device"]:
                stats["pending"] += 1
                return self.reply(400, {"error": "authorization_pending"})
            token = "access-%d" % (len(tokens) + 1)
            tokens.add(token)
            return self.reply(200, {"accessToken": token, "expiresIn": 3600, "refreshToken": "refresh-1"})
        if url.path == "/":
            form = {k: v[0] for k, v in parse_qs(raw.decode()).items()}
            auth = self.headers.get("Authorization", "")
            if form.get("Action") != "AssumeRole" or "Credential=AKIASOURCE/" not in auth or "/sts/aws4_request" not in auth:
                return self.reply(403, "<ErrorResponse><Error><Message>Bad request</Message></Error></ErrorResponse>", "text/xml")
            if form.get("SerialNumber") and form.get("TokenCode") != "123456":
                return self.reply(403, "<ErrorResponse><Error><Message>MultiFactorAuthentication failed</Message></Error></ErrorResponse>", "text/xml")
            stats["assumed"] += 1
            expires = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + 3600))
            return self.reply(200, f"""<AssumeRoleResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">
  <AssumeRoleResult><Credentials>
    <AccessKeyId>ASIAROLEEXAMPLE</AccessKeyId><SecretAccessKey>role-secret</SecretAccessKey>
    <SessionToken>role-session-token</SessionToken><Expiration>{expires}</Expiration>
  </Credentials></AssumeRoleResult></AssumeRoleResponse>""", "text/xml")
        self.reply(404, {"message": "Not Found"})


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
