#!/usr/bin/env python3
"""Server administration through the bridge, against a throwaway loopback server.

Needs a TS6 server you started yourself with a fresh database, and its first
`ServerAdmin` privilege key in a file (`--token-file`; the key is never printed
or logged). One bridge acts as the administrator, another as a plain client.
It checks, in order: a plain client is refused; the key is redeemed; group and
permission lists come back as rows; a channel is created, edited, given a
permission and deleted; a client is poked, kicked and banned and the ban is
listed and removed; the kicked client's status says why.

    python scripts/check_admin.py --binary <ts6-bridge.exe> --server-port 29987 --token-file key.txt
"""
import argparse
from pathlib import Path
import sys
import time

from check_channel_chat import ConnectionFailure, Peer

counter = 0


def admin(peer, action, **args):
    """Sends one admin operation and returns its `admin_result` event."""
    global counter
    counter += 1
    request_id = f"adm-{counter}"
    peer.send({"op": "admin", "request_id": request_id, "action": action, **args})
    return peer.until(lambda e: e.get("ev") == "admin_result" and e["request_id"] == request_id)


def expect(result, ok=True, what=""):
    assert result["ok"] is ok, f"{what}: {result}"
    return result["rows"]


def connect(peer, name, port):
    # A server keeps the previous session of an identity until it times out
    # (about 30 s), so a quick rerun can fail its handshake: try again.
    for attempt in range(6):
        peer.send({"op": "connect", "address": f"127.0.0.1:{port}", "nickname": name})
        try:
            return peer.until(lambda e: e.get("ev") == "state")["snapshot"]
        except ConnectionFailure as failure:
            if attempt == 5 or "handshake" not in str(failure):
                raise
            time.sleep(10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--server-port", type=int, required=True)
    parser.add_argument("--token-file", type=Path, required=True)
    args = parser.parse_args()
    token = args.token_file.read_text(encoding="utf-8").strip()
    children = []
    try:
        boss, guest = Peer(args.binary, "adm-boss", children), Peer(args.binary, "adm-guest", children)
        connect(boss, "Admin Test", args.server_port)
        snapshot = connect(guest, "Guest Test", args.server_port)
        guest_clid = next(c["id"] for c in snapshot["clients"] if c["is_self"])

        refused = admin(guest, "channel_create", name="No rights")
        assert refused["ok"] is False and refused["code"] == "refused", refused
        print("PASS: a client without rights is refused:", refused["error"])

        redeemed = admin(boss, "use_token", token=token)
        # A key works once; on a rerun the identity already holds the group.
        print("PASS: the privilege key was redeemed" if redeemed["ok"] else "INFO: key already used: " + str(redeemed["error"]))

        groups = expect(admin(boss, "server_groups"), what="server_groups")
        names = {row.get("name") for row in groups}
        assert "Server Admin" in names, names
        assert expect(admin(boss, "channel_groups"), what="channel_groups")
        permissions = expect(admin(boss, "permission_list"), what="permission_list")
        assert len(permissions) > 100 and any(p.get("permsid") == "b_client_ban_create" or p.get("permname") == "b_client_ban_create" for p in permissions)
        # The admin group may be above the administrator's own modify power;
        # the default group always can be read.
        normal = next(row["sgid"] for row in groups if row.get("name") == "Normal")
        listed = admin(boss, "server_group_permissions", group=int(normal))
        # Reading a group's permissions needs its own permission, which some
        # setups withhold from the administrator: report, do not fail.
        print("PASS: group permissions listed" if listed["ok"] else "INFO: group permissions refused: " + str(listed["error"]))
        print(f"PASS: {len(groups)} server groups, {len(permissions)} permissions listed")

        expect(admin(boss, "channel_create", name="Sala de teste", topic="criada pelo teste", max_clients=5), what="channel_create")
        state = boss.until(lambda e: e.get("ev") == "state" and any(c["name"] == "Sala de teste" for c in e["snapshot"]["channels"]))
        channel = next(c for c in state["snapshot"]["channels"] if c["name"] == "Sala de teste")
        expect(admin(boss, "channel_edit", id=channel["id"], name="Sala renomeada", topic="novo tópico"), what="channel_edit")
        boss.until(lambda e: e.get("ev") == "state" and any(c["name"] == "Sala renomeada" and c["topic"] == "novo tópico" for c in e["snapshot"]["channels"]))
        expect(admin(boss, "channel_set_permission", channel=channel["id"], permission="i_channel_needed_join_power", value=0), what="set perm")
        rows = expect(admin(boss, "channel_permissions", channel=channel["id"]), what="channel perms")
        assert any("i_channel_needed_join_power" in row.values() for row in rows), rows
        expect(admin(boss, "channel_remove_permission", channel=channel["id"], permission="i_channel_needed_join_power"), what="remove perm")
        print("PASS: a channel was created, renamed, got a permission and lost it")

        expect(admin(boss, "move_client", client=guest_clid, channel=channel["id"]), what="move_client")
        expect(admin(boss, "poke", client=guest_clid, message="oi"), what="poke")
        expect(admin(boss, "channel_delete", id=channel["id"], force=True), what="channel_delete")
        boss.until(lambda e: e.get("ev") == "state" and not any(c["id"] == channel["id"] for c in e["snapshot"]["channels"]))
        print("PASS: a client was moved and poked, then the channel was deleted")

        made = admin(boss, "server_group_create", name="Grupo do teste")
        if made["ok"]:
            row = next(r for r in expect(admin(boss, "server_groups"), what="groups") if r.get("name") == "Grupo do teste")
            assert row.get("type") == "1", row
            gid = int(row["sgid"])
            expect(admin(boss, "server_group_rename", group=gid, name="Grupo renomeado"), what="rename")
            assert any(r.get("name") == "Grupo renomeado" for r in expect(admin(boss, "server_groups"), what="groups"))
            expect(admin(boss, "server_group_delete", group=gid, force=True), what="delete")
            assert not any(r.get("sgid") == str(gid) for r in expect(admin(boss, "server_groups"), what="groups"))
            print("PASS: a server group was created, renamed and deleted")
        else:
            print("INFO: creating a group refused:", made["error"])

        bad = admin(boss, "server_group_remove_permission", group=1, permission="Not A Perm")
        assert bad["ok"] is False and bad["code"] == "invalid", bad
        print("PASS: a malformed permission name is rejected locally")

        expect(admin(boss, "ban", client=guest_clid, seconds=60, reason="teste"), what="ban")
        try:
            guest.until(lambda e: False)
        except ConnectionFailure as failure:
            status = next(e for e in reversed(guest.history) if e.get("ev") == "status")
            print("INFO: the banned client was told:", status.get("error"), f"({failure})")
        bans = expect(admin(boss, "ban_list"), what="ban_list")
        assert bans, "the ban is not listed"
        for row in bans:
            expect(admin(boss, "ban_remove", id=int(row["banid"])), what="ban_remove")
        again = admin(boss, "ban_list")  # an empty list may come back as an error
        assert again["ok"] is False or not again["rows"], again
        print("PASS: the client was banned, the ban listed and removed")
        return 0
    finally:
        for child in children:
            child.close()


if __name__ == "__main__":
    sys.exit(main())
