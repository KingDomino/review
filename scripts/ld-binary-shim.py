#!/usr/bin/env python3
# Emulates `ld -r -b binary -z noexecstack -o OUT IN` for cross builds (MuPDF font embedding).
import os, re, subprocess, sys
args = sys.argv[1:]
if "-b" in args and "binary" in args:
    out = args[args.index("-o") + 1]
    inp = [a for a in args if not a.startswith("-") and a not in ("binary", "noexecstack", out) and os.path.exists(a)][-1]
    sym = "_binary_" + re.sub(r"[^A-Za-z0-9]", "_", inp)
    target = os.environ["REVIEW_ZIG_TARGET"]
    asm = out + ".s"
    with open(asm, "w") as f:
        f.write(f""".section .rodata
.global {sym}_start
.global {sym}_end
.global {sym}_size
.balign 16
{sym}_start:
.incbin "{os.path.abspath(inp)}"
{sym}_end:
.byte 0
.set {sym}_size, {sym}_end - {sym}_start
.section .note.GNU-stack,"",%progbits
""")
    r = subprocess.call([os.environ["REVIEW_ZIG"], "cc", "-target", target, "-c", asm, "-o", out])
    os.remove(asm)
    sys.exit(r)
os.execv("/usr/bin/ld", ["/usr/bin/ld"] + args)
