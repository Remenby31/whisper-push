"""Pack PNGs into a Windows .ico.

The .ico is the app's face on Windows: the .exe's icon in Explorer, Task Manager
and alt-tab (embedded by build.rs), the Start Menu shortcut, and the entry in
Add/Remove Programs. It is committed (wix/whisper-push.ico) like the DMG artwork
is; re-derive it from the brand master with `make windows-icon`.

Vista+ .ico files may hold PNG blobs directly, so this needs no image library —
just the already-resized PNGs.

    python3 scripts/make-ico.py out.ico 16:16.png 32:32.png ...
"""
import struct, sys

def build(pngs, out):
    n = len(pngs)
    header = struct.pack('<HHH', 0, 1, n)          # reserved, type=icon, count
    offset = 6 + 16 * n
    entries, blobs = b'', b''
    for size, data in pngs:
        w = 0 if size >= 256 else size
        entries += struct.pack('<BBBBHHII', w, w, 0, 0, 1, 32, len(data), offset)
        blobs += data
        offset += len(data)
    open(out, 'wb').write(header + entries + blobs)

if __name__ == '__main__':
    out = sys.argv[1]
    pngs = [(int(a.split(':')[0]), open(a.split(':')[1], 'rb').read()) for a in sys.argv[2:]]
    build(pngs, out)
