"""Time seqpeeq and its peers opening the benchmark files: the terminal
viewers termal, alv, samtools tview and ASCIIGenome, and the GUI programs
IGV, FigTree and Jalview.

Each run is timed from launch until the tool has shown the file:
- IGV, FigTree and Jalview until their script mode has finished and they exit;
- the terminal viewers until their first screen of data is drawn in a
  pseudo-terminal; alv, which prints the whole alignment and exits, is piped
  into `less -R`, the way it is used.

Peak memory is read from the operating system when each run exits.

    python bench/run.py --data bench/data --out bench/results.json --only all
"""

import argparse
import fcntl
import re
import json
import os
import platform
import select
import signal
import statistics
import struct
import subprocess
import sys
import tempfile
import termios
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Callable

import pyte

ROOT = Path(__file__).resolve().parent.parent
ENVS = Path(os.environ.get("SEQPEEQ_BENCH_ENVS", Path.home() / "miniconda3" / "envs"))
BENCH = ENVS / "seqpeeq-bench"
BENCH_IGV = ENVS / "seqpeeq-bench-igv"
BENCH_ASCII = ENVS / "seqpeeq-bench-asciigenome"
SEQPEEQ = ROOT / "target" / "release" / "seqpeeq"
# termal is not on Bioconda: `cargo install termal-msa --root bench/tools`.
TERMAL = ROOT / "bench" / "tools" / "bin" / "termal"
MEMORY = "-Xmx16g"
TIMEOUT = 600
LONG_RUN = 60
PTY_ROWS, PTY_COLS = 48, 160
# The stretch of a BAM both IGV and seqpeeq are taken to and timed showing.
WINDOW = "c1:1-1000"
# The coarsest scale seqpeeq fits WINDOW to in PTY_COLS columns. A small
# contig opens zoomed out with a depth already shown, at a coarser scale.
WINDOW_SCALE = 10
LEFT_BEHIND_GRACE = 0.5
# What each terminal tool is sent to quit, when it isn't `q`: ASCIIGenome
# reads a command line, ended by Enter.
QUIT = {"asciigenome": b"q\r"}

# Which tools are timed on which kind of file.
TOOLS = {"msa": ["seqpeeq", "termal", "alv", "jalview"], "tree": ["seqpeeq", "figtree"], "bam": ["seqpeeq", "tview", "asciigenome", "igv"]}
FILES = {
    "tiny": [("msa", "tiny.fa"), ("tree", "tiny.nwk"), ("bam", "tiny.bam")],
    "small": [("msa", "aln_1k.fa"), ("tree", "tree_1k.nwk"), ("bam", "reads_10x.bam")],
    "all": [("msa", "aln_1k.fa"), ("msa", "aln_10k.fa"), ("msa", "aln_30k.fa"), ("msa", "aln_100k.fa"),
            ("tree", "tree_1k.nwk"), ("tree", "tree_10k.nwk"), ("tree", "tree_100k.nwk"),
            ("tree", "tree_1m.nwk"),
            ("bam", "reads_10x.bam"), ("bam", "reads_100x.bam"), ("bam", "reads_1000x.bam")],
}


@dataclass
class Run:
    seconds: float
    max_rss: int
    outcome: str  # "ok", "timed out", "out of memory" or "failed"
    detail: str = ""


def _rss_bytes(rusage):
    # macOS reports bytes, Linux kibibytes.
    return rusage.ru_maxrss if sys.platform == "darwin" else rusage.ru_maxrss * 1024


# A terminal escape sequence: CSI (ESC [ … final byte) or ESC and one character.
ESCAPE = re.compile(r"\x1b(\[[0-?]*[ -/]*[@-~]|.)")


def publishable(text) -> str:
    """`text` fit for the public page: no escape codes, and ~ for the home folder."""
    return ESCAPE.sub("", text).replace(str(Path.home()), "~")


def _ended(status, output, seconds, rusage):
    text = publishable(output.decode(errors="replace"))
    if "OutOfMemoryError" in text:
        return Run(seconds, _rss_bytes(rusage), "out of memory", text[-300:])
    code = os.waitstatus_to_exitcode(status)
    if code != 0:
        return Run(seconds, _rss_bytes(rusage), "failed", f"exit {code}: {text[-300:]}")
    return Run(seconds, _rss_bytes(rusage), "ok")


def _kill(pid):
    try:
        os.killpg(pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        # Gone, or (on macOS) only its unreaped leader is left.
        pass


def _left_behind(pgid):
    """Whether processes of the group outlive its leader. A launcher that
    starts the real program and exits would otherwise be timed instead of
    the program; any found are killed."""
    deadline = time.monotonic() + LEFT_BEHIND_GRACE
    while True:
        try:
            os.killpg(pgid, 0)
        except (ProcessLookupError, PermissionError):
            return False
        if time.monotonic() > deadline:
            _kill(pgid)
            return True
        time.sleep(0.02)


def measure(argv, *, timeout, env=None, ready=None, pty_size=(PTY_ROWS, PTY_COLS), quit=b"q") -> Run:
    """Run `argv` once in its own process group and say how it went.

    Without `ready`, the time is until the program exits. With `ready`,
    the program runs in a pseudo-terminal and the time is until the screen
    passes the last of its steps; it is then sent `q` to quit. `ready` is a
    test of the screen's text, or a list of (test, keys) steps: when a
    step's test holds its keys are typed, and the last step has keys None.
    `quit` is what is typed once the last step holds.
    On `timeout` the whole process group is killed, and a group that
    outlives its leader is a failure.
    """
    if ready is None:
        return _measure_exit(argv, timeout, env)
    return _measure_screen(argv, timeout, env, ready, pty_size, quit)


def _measure_exit(argv, timeout, env):
    with tempfile.TemporaryFile() as out:
        started = time.monotonic()
        proc = subprocess.Popen(argv, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                                env=env, start_new_session=True)
        while True:
            pid, status, rusage = os.wait4(proc.pid, os.WNOHANG)
            seconds = time.monotonic() - started
            if pid:
                break
            if seconds > timeout:
                _kill(proc.pid)
                _, status, rusage = os.wait4(proc.pid, 0)
                proc.returncode = -9
                return Run(seconds, _rss_bytes(rusage), "timed out", f"after {timeout:g} s")
            time.sleep(0.005)
        proc.returncode = os.waitstatus_to_exitcode(status)
        out.seek(0)
        if _left_behind(proc.pid):
            return Run(seconds, _rss_bytes(rusage), "failed",
                       "exited with processes still running; they were killed")
        return _ended(status, out.read(), seconds, rusage)


def _measure_screen(argv, timeout, env, ready, size, quit):
    steps = ready if isinstance(ready, list) else [(ready, None)]
    step = 0
    rows, cols = size
    master, slave = os.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    env = dict(env or os.environ, TERM="xterm-256color")
    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)
    output = bytearray()
    started = time.monotonic()
    proc = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave, env=env,
                            start_new_session=True, close_fds=True)
    os.close(slave)
    shown = None
    quit_sent = None
    status = rusage = None
    try:
        while True:
            now = time.monotonic()
            if shown is None and now - started > timeout:
                _kill(proc.pid)
                _, status, rusage = os.wait4(proc.pid, 0)
                return Run(now - started, _rss_bytes(rusage), "timed out", f"after {timeout:g} s")
            if quit_sent is not None:
                if now - quit_sent > 10:
                    _kill(proc.pid)
                elif now - quit_sent > 2:
                    os.write(master, b"y")  # answer a "quit anyway?" question
            pid, status, rusage = os.wait4(proc.pid, os.WNOHANG)
            if pid:
                break
            readable, _, _ = select.select([master], [], [], 0.01)
            if readable:
                try:
                    chunk = os.read(master, 65536)
                except OSError:
                    chunk = b""
                output += chunk
                stream.feed(chunk)
                while shown is None and steps[step][0]("\n".join(screen.display)):
                    keys = steps[step][1]
                    if keys is None:
                        shown = time.monotonic() - started
                        quit_sent = time.monotonic()
                        os.write(master, quit)
                    else:
                        os.write(master, keys)
                        step += 1
    finally:
        os.close(master)
    proc.returncode = os.waitstatus_to_exitcode(status)
    if _left_behind(proc.pid):
        return Run(time.monotonic() - started, _rss_bytes(rusage), "failed",
                   "exited with processes still running; they were killed")
    if shown is None:
        run = _ended(status, bytes(output), time.monotonic() - started, rusage)
        if run.outcome == "ok":
            return Run(run.seconds, run.max_rss, "failed",
                       "exited before showing the file: "
                       + publishable("\n".join(screen.display).strip())[-300:])
        return run
    return Run(shown, _rss_bytes(rusage), "ok")


def screen_shows(text) -> Callable[[str], bool]:
    return lambda screen: text in screen


def seqpeeq_ready(kind) -> Callable[[str], bool]:
    """When seqpeeq has drawn the file: `opened …` for an alignment or a
    tree; for a BAM, the depth on the bottom row, which is only shown once
    the reads in view have been read."""
    if kind == "bam":
        return lambda screen: "depth " in screen
    return screen_shows("opened ")


def seqpeeq_steps(kind):
    """How seqpeeq is driven to show a file: for an alignment or a tree,
    until it is opened; a BAM opens on the whole contig, so it is then
    taken to IGV's window with `g` and timed until those reads are read."""
    if kind == "bam":
        return [(screen_shows("opened "), f"g{WINDOW}\r".encode()), (_window_shown, None)]
    return [(seqpeeq_ready(kind), None)]


def _window_shown(screen):
    """The reads of WINDOW are drawn: a depth is shown at its scale."""
    scale = re.search(r"([\d,]+) bp a column", screen)
    return (seqpeeq_ready("bam")(screen) and scale is not None
            and int(scale.group(1).replace(",", "")) <= WINDOW_SCALE)


def alv_ready(screen) -> bool:
    """When less shows alv's first screen: a sequence row above less's
    prompt, which is drawn last."""
    rows = [row.rstrip() for row in screen.splitlines() if row.strip()]
    if not rows or rows[-1] not in (":", "(END)"):
        return False
    return any(re.fullmatch(r"\S+ +[A-Za-z*.-]+", row) for row in rows[:-1])


def termal_ready(screen) -> bool:
    """When termal has drawn its first frame: the first sequence's row, and
    the help line at the bottom, which is drawn last."""
    return re.search(r"│ *1│", screen) is not None and "Press '?' for help" in screen


def tview_ready(screen) -> bool:
    """When samtools tview has drawn the window: its ruler, the reference
    under it, and reads below those."""
    lines = screen.splitlines()
    return (len(lines) > 2 and lines[0][:1].isdigit() and lines[1].strip() != ""
            and any(line.strip() for line in lines[2:]))


def asciigenome_ready(screen) -> bool:
    """When ASCIIGenome has drawn the window: its read track, and the footer
    naming the window, which is drawn after the tracks."""
    return "; Reads: " in screen and f"\n{WINDOW}; " in screen


def _java(env):
    return str(env / "lib" / "jvm" / "bin" / "java")


def _reference(bam: Path) -> Path:
    return bam.with_name("tiny_ref.fa" if bam.name == "tiny.bam" else "ref.fa")


def commands(tool, file: Path, scratch: Path) -> list[str]:
    """The command line timing `tool` on `file`; `scratch` takes its outputs."""
    file = Path(file).resolve()
    scratch = Path(scratch).resolve()
    scratch.mkdir(parents=True, exist_ok=True)
    if tool == "seqpeeq":
        return [str(SEQPEEQ), "--no-log", str(file)]
    if tool == "termal":
        return [str(TERMAL), str(file)]
    if tool == "alv":
        # alv prints the whole alignment and exits, so it is piped into less,
        # as it is used. less reads no settings from the environment and
        # writes no history. The file is an argument, never part of the script.
        script = ('LESS= LESSOPEN= LESSHISTFILE=-; export LESS LESSOPEN LESSHISTFILE; '
                  'exec "$0" -k "$1" | less -R')
        return ["sh", "-c", script, str(BENCH / "bin" / "alv"), str(file)]
    if tool == "tview":
        start = WINDOW.split("-")[0]
        return [str(BENCH / "bin" / "samtools"), "tview", "-p", start, str(file), str(_reference(file))]
    if tool == "asciigenome":
        # The jar run directly: Bioconda's wrapper script would be timed, and
        # it sets its own heap size. Its history goes to the scratch folder.
        return [_java(BENCH_ASCII), MEMORY, f"-Duser.home={scratch}", "-jar",
                str(BENCH_ASCII / "bin" / "ASCIIGenome.jar"), "-r", WINDOW, "-fa", str(_reference(file)), str(file)]
    if tool == "figtree":
        jar = _jar("figtree-*/figtree.jar")
        return [_java(BENCH), MEMORY, "-jar", str(jar), "-graphic", "PNG", "-width", "1200",
                "-height", "800", str(file), str(scratch / "figtree.png")]
    if tool == "jalview":
        jar = _jar("jalview-*/jalview-all-j11.jar")
        # The jar's own entry point is a launcher that starts Jalview in a
        # second JVM and exits; this is the command it runs.
        # Its settings and recent files go to the scratch folder, not ~.
        return [_java(BENCH), MEMORY, "-Dapple.awt.UIElement=true", f"-Duser.home={scratch}",
                "-cp", str(jar),
                "jalview.bin.Jalview", "--headless", "--open", str(file), "--close"]
    if tool == "igv":
        ref = _reference(file)
        prefs = scratch / "igv" / "prefs.properties"
        prefs.parent.mkdir(parents=True, exist_ok=True)
        # The benchmark reference as the default genome, so IGV does not
        # download hg38 when it starts.
        prefs.write_text(f"DEFAULT_GENOME_KEY={ref}\n")
        batch = scratch / "batch.txt"
        batch.write_text("\n".join(["new", f"genome {ref}", f"load {file}", f"goto {WINDOW}",
                                    f"snapshotDirectory {scratch}", "snapshot igv.png", "exit", ""]))
        lib = BENCH_IGV / "lib" / "igv"
        return [_java(BENCH_IGV), f"--module-path={lib}", MEMORY, f"@{lib / 'igv.args'}",
                "-Dsamjdk.snappy.disable=true", "-Djava.net.preferIPv4Stack=true",
                f"-Duser.home={scratch}", "--module=org.igv/org.broad.igv.ui.Main", "-b", str(batch)]
    raise ValueError(f"no such tool: {tool}")


def _ready_for(tool, kind):
    return {"seqpeeq": seqpeeq_steps(kind), "termal": termal_ready, "alv": alv_ready, "tview": tview_ready, "asciigenome": asciigenome_ready}.get(tool)


def _output(argv):
    try:
        return subprocess.run(argv, capture_output=True, text=True, timeout=30).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def machine():
    if sys.platform == "darwin":
        memory = int(_output(["sysctl", "-n", "hw.memsize"]) or 0)
        return {"chip": _output(["sysctl", "-n", "machdep.cpu.brand_string"]),
                "cores": int(_output(["sysctl", "-n", "hw.ncpu"]) or 0),
                "memory_bytes": memory,
                "os": f"macOS {_output(['sw_vers', '-productVersion'])}"}
    return {"chip": platform.processor(), "cores": os.cpu_count(), "memory_bytes": 0,
            "os": platform.platform()}


def architecture(program) -> str:
    """The CPU a program is built for, as platform.machine() names it."""
    kind = _output(["file", "-b", str(program)])
    return next((arch for arch in ["arm64", "x86_64"] if arch in kind), kind)


def versions():
    found = {}
    for name, program in [("seqpeeq", SEQPEEQ), ("termal", TERMAL)]:
        if not program.exists():
            continue
        version = _output([str(program), "--version"]).split()[-1:]
        found[name] = f"{' '.join(version)} ({architecture(program)})"
    javas = {BENCH: "FigTree, Jalview", BENCH_IGV: "IGV", BENCH_ASCII: "ASCIIGenome"}
    for env, names in [(BENCH, ["samtools", "alv", "figtree", "jalview", "openjdk"]), (BENCH_IGV, ["igv", "openjdk"]),
                       (BENCH_ASCII, ["asciigenome", "openjdk"])]:
        for name in names:
            meta = sorted((env / "conda-meta").glob(f"{name}-[0-9]*.json"))
            if meta:
                version = meta[-1].name.removeprefix(f"{name}-").rsplit("-", 1)[0]
                found[name if name != "openjdk" else f"java ({javas[env]})"] = version
    return found


def _jar(pattern) -> Path:
    """The jar in the bench environment matching `pattern`, or where it would be."""
    return next((BENCH / "share").glob(pattern), BENCH / "share" / pattern)


def _programs(tool) -> list[Path]:
    """The files `tool` needs to run."""
    return {
        "seqpeeq": [SEQPEEQ],
        "termal": [TERMAL],
        "alv": [BENCH / "bin" / "alv"],
        "tview": [BENCH / "bin" / "samtools"],
        "asciigenome": [Path(_java(BENCH_ASCII)), BENCH_ASCII / "bin" / "ASCIIGenome.jar"],
        "jalview": [Path(_java(BENCH)), _jar("jalview-*/jalview-all-j11.jar")],
        "figtree": [Path(_java(BENCH)), _jar("figtree-*/figtree.jar")],
        "igv": [Path(_java(BENCH_IGV)), BENCH_IGV / "lib" / "igv" / "igv.args"],
    }[tool]


def missing_tools(tools) -> list[str]:
    """The tools among `tools` that are not installed."""
    return [tool for tool in tools if not all(p.exists() for p in _programs(tool))]


def time_one(tool, kind, file: Path, scratch: Path, repeats, timeout):
    runs = []
    for _ in range(repeats):
        got = measure(commands(tool, file, scratch), timeout=timeout, ready=_ready_for(tool, kind),
                      quit=QUIT.get(tool, b"q"))
        runs.append(got)
        print(f"  {tool:8} {file.name:16} {got.outcome:13} {got.seconds:8.2f} s "
              f"{got.max_rss / 1e6:9.0f} MB", flush=True)
        if got.outcome != "ok" or got.seconds > LONG_RUN:
            break
    ok = [r.seconds for r in runs if r.outcome == "ok"]
    return {
        "tool": tool, "kind": kind, "file": file.name, "file_bytes": file.stat().st_size,
        "runs": [asdict(r) for r in runs],
        "outcome": "ok" if ok else runs[-1].outcome,
        "median_seconds": statistics.median(ok) if ok else None,
        "max_rss": max(r.max_rss for r in runs),
    }


def main(argv=None):
    args = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    args.add_argument("--data", required=True, type=Path)
    args.add_argument("--out", required=True, type=Path)
    args.add_argument("--only", choices=list(FILES), default="all")
    args.add_argument("--tools", nargs="*", default=["seqpeeq", "termal", "alv", "tview", "asciigenome", "igv", "figtree", "jalview"])
    args.add_argument("--repeats", type=int, default=3)
    args.add_argument("--timeout", type=float, default=TIMEOUT)
    args = args.parse_args(argv)
    known = {tool for tools in TOOLS.values() for tool in tools}
    unknown = [tool for tool in args.tools if tool not in known]
    if unknown:
        sys.exit(f"no such tool: {', '.join(unknown)}")
    missing = missing_tools(args.tools)
    if missing:
        sys.exit(f"missing: {', '.join(missing)}")
    files = FILES["tiny"] + (FILES[args.only] if args.only != "tiny" else [])
    report = {"machine": machine(), "versions": versions(), "timeout_seconds": args.timeout,
              "memory_limit": MEMORY.removeprefix("-Xmx"), "results": []}
    with tempfile.TemporaryDirectory(prefix="seqpeeq-bench-") as scratch:
        for kind, name in files:
            for tool in TOOLS[kind]:
                if tool not in args.tools:
                    continue
                result = time_one(tool, kind, args.data / name, Path(scratch) / tool,
                                  args.repeats, args.timeout)
                report["results"].append(result)
                args.out.write_text(json.dumps(report, indent=1) + "\n")


if __name__ == "__main__":
    main()
