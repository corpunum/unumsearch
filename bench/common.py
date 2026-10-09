# SPDX-License-Identifier: Apache-2.0
"""Shared helpers for the race/cold benchmarks: the corpus definition and roots come from the
unumsearch config file, so rg and unumsearch search exactly the same files."""
import os, subprocess, sys

try:
    import tomllib
except ImportError:  # Python < 3.11
    sys.exit("python 3.11+ required (tomllib)")


def default_config():
    if os.environ.get("UNUMSEARCH_CONFIG"):
        return os.environ["UNUMSEARCH_CONFIG"]
    if sys.platform == "darwin":
        base = os.path.expanduser("~/Library/Application Support")
    elif os.name == "nt":
        base = os.environ.get("APPDATA", "")
    else:
        base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    return os.path.join(base, "unumsearch", "config.toml")


def roots(config):
    with open(config, "rb") as f:
        cfg = tomllib.load(f)
    return sorted({os.path.normpath(os.path.expanduser(r)) for r in cfg.get("roots", [])})


def cli_prefix(binary, config, index_dir):
    """The unumsearch command line for this config (and a private index, if given)."""
    cmd = [binary, "--config", config]
    return cmd + (["--index-dir", index_dir] if index_dir else [])


def ignore_file(prefix, path):
    """`unumsearch excludes` as an rg --ignore-file: the same corpus rules for both."""
    with open(path, "w") as f:
        f.write(subprocess.run(prefix + ["excludes"], capture_output=True, text=True, check=True).stdout)
    return path


def proc_status(pid):
    """VmHWM / VmRSS / RssAnon of a process in kB (Linux), or {}."""
    out = {}
    try:
        for line in open(f"/proc/{pid}/status"):
            k, _, v = line.partition(":")
            if k in ("VmHWM", "VmRSS", "RssAnon", "RssFile"):
                out[k] = int(v.split()[0])
    except OSError:
        pass
    return out
