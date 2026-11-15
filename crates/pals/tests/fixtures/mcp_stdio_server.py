"""A minimal MCP server over stdio for the inventory adapter's tests.

Standard library only. It speaks newline-delimited JSON-RPC on stdin and
stdout and answers only `initialize` and `tools/list`, page by page; any
other request gets a JSON-RPC error, and notifications get nothing. It
writes only the files its arguments name, which the tests keep in their own
temporary directories.

Arguments:

  --tools a,b,c        the tools it lists, in order (default: read_file,list_dir)
  --tools-file FILE    read that list from FILE instead, when it starts
  --page-size N        tools per page of tools/list (default: all in one page)
  --endless            after its tools, list `tool-<n>` pages without end,
                       always with a next cursor
  --hang               read requests and never answer any
  --ready FILE         create FILE once it is reading requests
  --requests FILE      append the method of each message it reads, one per line
  --env-file FILE      write the names of the environment it was started
                       with, one per line, then `pid <its pid>` and
                       `grandchild <pid>` of a child it starts that sleeps
  --value-of NAME      with --value-file, write the value of NAME there
  --value-file FILE
"""

import argparse
import json
import os
import subprocess
import sys


def parse_args():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tools", default="read_file,list_dir")
    parser.add_argument("--tools-file")
    parser.add_argument("--page-size", type=int, default=0)
    parser.add_argument("--endless", action="store_true")
    parser.add_argument("--hang", action="store_true")
    parser.add_argument("--ready")
    parser.add_argument("--requests")
    parser.add_argument("--env-file")
    parser.add_argument("--value-of")
    parser.add_argument("--value-file")
    return parser.parse_args()


def environment_names():
    """The names of the environment the process was started with: from
    /proc/self/environ, which Python's own changes (such as its C locale
    coercion) do not touch; else os.environ."""
    try:
        with open("/proc/self/environ", "rb") as environ:
            block = environ.read()
        return [
            entry.split(b"=", 1)[0].decode("utf-8", "replace")
            for entry in block.split(b"\0")
            if entry
        ]
    except OSError:
        return list(os.environ)


def write_environment(path):
    # Leaves the group only if killed with it: it stays in this one.
    grandchild = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(3600)"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    lines = environment_names()
    lines.append("pid %d" % os.getpid())
    lines.append("grandchild %d" % grandchild.pid)
    temporary = path + ".partial"
    with open(temporary, "w") as out:
        out.write("\n".join(lines) + "\n")
    os.replace(temporary, path)


def tool(name):
    return {
        "name": name,
        "description": "The %s tool of the fixture" % name,
        "inputSchema": {"type": "object"},
        "annotations": {"title": "Fixture %s" % name, "readOnlyHint": True},
    }


def page_of(tools, size, endless, cursor):
    """The tools of the page at `cursor` (an offset, as text) and the
    cursor of the next one, if any."""
    start = int(cursor) if cursor else 0
    size = size if size > 0 else max(len(tools), 1)
    if endless:
        names = [
            tools[n] if n < len(tools) else "tool-%d" % n
            for n in range(start, start + size)
        ]
        return [tool(name) for name in names], str(start + size)
    names = tools[start : start + size]
    following = start + size
    return [tool(name) for name in names], (
        str(following) if following < len(tools) else None
    )


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


def main():
    args = parse_args()
    if args.tools_file:
        with open(args.tools_file) as listed:
            text = listed.read().strip()
    else:
        text = args.tools
    tools = [name for name in text.split(",") if name]
    if args.value_of and args.value_file:
        with open(args.value_file, "w") as out:
            out.write(os.environ.get(args.value_of, ""))
    if args.env_file:
        write_environment(args.env_file)
    requests = open(args.requests, "a") if args.requests else None
    if args.ready:
        open(args.ready, "w").close()

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except ValueError:
            continue
        method = message.get("method")
        if requests is not None:
            requests.write("%s\n" % method)
            requests.flush()
        if args.hang or "id" not in message or method is None:
            continue
        if method == "initialize":
            params = message.get("params") or {}
            send(
                {
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": {
                        "protocolVersion": params.get("protocolVersion", "2025-06-18"),
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "pals-test-fixture", "version": "1"},
                    },
                }
            )
        elif method == "tools/list":
            cursor = (message.get("params") or {}).get("cursor")
            listed, following = page_of(tools, args.page_size, args.endless, cursor)
            result = {"tools": listed}
            if following is not None:
                result["nextCursor"] = following
            send({"jsonrpc": "2.0", "id": message["id"], "result": result})
        else:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "error": {"code": -32601, "message": "method not found"},
                }
            )


if __name__ == "__main__":
    main()
