"""Run the ignored Rust NAS integration test against a temporary read-only SFTP server.
Requires Python 3 and paramiko. Uses synthetic cells; never contacts the laboratory.
Run from the repository root: python tests/nas_sftp_fixture.py
"""
import base64
import hashlib
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading

import paramiko


class Auth(paramiko.ServerInterface):
    def check_auth_password(self, username, password):
        return paramiko.AUTH_SUCCESSFUL if (username, password) == ("fixture", "fixture") else paramiko.AUTH_FAILED

    def get_allowed_auths(self, username):
        return "password"

    def check_channel_request(self, kind, chanid):
        return paramiko.OPEN_SUCCEEDED if kind == "session" else paramiko.OPEN_FAILED_ADMINISTRATIVELY_PROHIBITED


class ReadOnly(paramiko.SFTPServerInterface):
    def __init__(self, server, *, root, audit):
        super().__init__(server)
        self.root = root
        self.audit = audit

    def local(self, path):
        result = (self.root / path.lstrip("/")).resolve()
        if not result.is_relative_to(self.root):
            raise PermissionError(path)
        return result

    def canonicalize(self, path):
        return "/" + str(self.local(path).relative_to(self.root))

    def list_folder(self, path):
        result = []
        for p in self.local(path).iterdir():
            attrs = paramiko.SFTPAttributes.from_stat(p.lstat())
            attrs.filename = p.name
            result.append(attrs)
        return result

    def stat(self, path):
        try:
            return paramiko.SFTPAttributes.from_stat(self.local(path).stat())
        except OSError as error:
            return paramiko.SFTPServer.convert_errno(error.errno)

    lstat = stat

    def open(self, path, flags, attr):
        self.audit.append((path, flags))
        if flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT | os.O_TRUNC | os.O_APPEND):
            return paramiko.SFTP_PERMISSION_DENIED
        handle = paramiko.SFTPHandle(flags)
        handle.readfile = self.local(path).open("rb")
        handle.stat = lambda: paramiko.SFTPAttributes.from_stat(os.fstat(handle.readfile.fileno()))
        return handle


def main():
    with tempfile.TemporaryDirectory(prefix="amanece-nas-fixture-") as tmp:
        root = Path(tmp).resolve()
        archive = root / "archive" / "user"
        archive.mkdir(parents=True)
        (archive / "latest.res").write_text("TITL synthetic fixture\nCELL 0.71073 10 20 30 90 100 90\nLATT -7\nHKLF 4\nEND\n")
        (archive / "latest.hkl").write_text("synthetic reflection fixture\n")
        (archive / "nested").mkdir()
        (archive / "nested" / "XDS_ASCII.HKL").write_text("!SPACE_GROUP_NUMBER= 19\n!UNIT_CELL_CONSTANTS= 11 22 33 90 90 90\n!END_OF_HEADER\n" + "0 0 0 0 0\n" * 100_000)
        (archive / "@eaDir").mkdir()
        (archive / "@eaDir" / "ignored.ins").write_text("invalid")
        (root / "outside").mkdir()
        (root / "outside" / "secret.res").write_text("must never be opened")
        (archive / "escape").symlink_to(root / "outside", target_is_directory=True)
        key = paramiko.RSAKey.generate(2048)
        audit = []
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        port = listener.getsockname()[1]
        fingerprint = "SHA256:" + base64.b64encode(hashlib.sha256(key.asbytes()).digest()).decode().rstrip("=")
        known_hosts = root / "known_hosts"
        known_hosts.write_text(f"127.0.0.1:{port} ssh-rsa {fingerprint}\n")
        transports = []

        def serve():
            while True:
                try:
                    client, _ = listener.accept()
                except OSError:
                    return
                transport = paramiko.Transport(client)
                transports.append(transport)
                transport.add_server_key(key)
                transport.set_subsystem_handler("sftp", paramiko.SFTPServer, ReadOnly, root=root, audit=audit)
                transport.start_server(server=Auth())

        threading.Thread(target=serve, daemon=True).start()
        try:
            env = os.environ | dict(NAS_HOST="127.0.0.1", NAS_PORT=str(port), NAS_USER="fixture", NAS_PASS="fixture", NAS_ROOT="/archive", NAS_GROUP="user", NAS_EXPECT_FILES="2", NAS_FORBIDDEN_PATH="/user/escape/secret.res", NAS_KNOWN_HOSTS=str(known_hosts))
            for sample, cell in [("latest.res", "10,20,30,90,100,90"), ("nested/XDS_ASCII.HKL", "11,22,33,90,90,90")]:
                subprocess.run(["cargo", "test", "--locked", "live_nas_readonly", "--", "--ignored", "--nocapture"], env=env | dict(NAS_SAMPLE=sample, NAS_EXPECT_CELL=cell), check=True)
            assert audit
            assert all(flags == os.O_RDONLY for _, flags in audit), audit
            assert all("@eaDir" not in path and "escape" not in path for path, _ in audit), audit
            print(f"Fixture passed: {len(audit)} read-only opens, SHELX and XDS cells verified.")
        finally:
            listener.close()
            for transport in transports:
                transport.close()


if __name__ == "__main__":
    main()
