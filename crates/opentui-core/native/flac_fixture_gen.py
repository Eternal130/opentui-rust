"""Generates the minimal FLAC fixture for opentui-core's audio tests.

A 16-bit mono 48 kHz stream: one 192-frame fixed-blocksize frame with a
VERBATIM subframe carrying the ramp i*100. No encoder exists in this
environment, so the bytes are laid out by hand per the FLAC format spec,
with two decoder-verified adjustments for the *vendored* dr_flac this
crate compiles (third_party/opentui/.../vendor/miniaudio/miniaudio.h):

- the frame-header CRC-8 is validated against dr_flac's own convention
  (it seeds 0xCE and skips the 14 sync bits, miniaudio.h:85783); for
  this fixed header that value is the constant 0xBA, verified by
  round-tripping through ma_dr_flac_open_memory_and_read_pcm_frames_f32;
- the frame CRC-16 is the spec CRC-16 (poly 0x8005, init 0) over the
  frame from the sync code up to (not including) the CRC-16 itself.
"""
import struct

SAMPLE_RATE, CHANNELS, BPS, BLOCK = 48000, 1, 16, 192

class BW:
    def __init__(self): self.bits = []
    def w(self, val, n):
        for i in range(n - 1, -1, -1): self.bits.append((val >> i) & 1)
    def bytes(self):
        assert len(self.bits) % 8 == 0
        out = bytearray()
        for i in range(0, len(self.bits), 8):
            b = 0
            for bit in self.bits[i:i + 8]: b = (b << 1) | bit
            out.append(b)
        return bytes(out)

def crc16(data):
    crc = 0
    for byte in data:
        crc ^= byte << 8
        for _ in range(8):
            crc = ((crc << 1) ^ 0x8005) & 0xFFFF if crc & 0x8000 else (crc << 1) & 0xFFFF
    return crc

# STREAMINFO (last metadata block)
si = BW()
si.w(BLOCK, 16); si.w(BLOCK, 16)      # min/max blocksize
si.w(0, 24); si.w(0, 24)              # min/max framesize: unknown
si.w(SAMPLE_RATE, 20); si.w(CHANNELS - 1, 3); si.w(BPS - 1, 5)
si.w(BLOCK, 36)                       # total samples
assert len(si.bytes()) == 18
hdr = BW(); hdr.w(1, 1); hdr.w(0, 7); hdr.w(34, 24)
streaminfo = hdr.bytes() + si.bytes() + bytes(16)  # md5: zeros

# frame: sync | 0 | fixed-blocksize | blocksize=192 | 48kHz | mono |
# 16-bit | utf8 frame# 0 | crc8
fh = BW()
fh.w(0b11111111111110, 14); fh.w(0, 1); fh.w(0, 1)
fh.w(0b0001, 4)                       # blocksize code: 192
fh.w(0b1001, 4)                       # sample rate code: 48kHz
fh.w(CHANNELS - 1, 4)                 # channel assignment: mono
fh.w(0b100, 3)                        # sample size: 16 bits
fh.w(0, 1)                            # reserved
fh.w(0, 8)                            # UTF-8 frame number 0
frame = bytearray(fh.bytes())
frame.append(0xBA)                    # dr_flac-verified header CRC-8

sf = BW()
sf.w(0, 1); sf.w(1, 6); sf.w(0, 1)    # VERBATIM subframe
for i in range(BLOCK):
    sf.w((i * 100) & 0x7FFF, BPS)
frame += sf.bytes()
frame += struct.pack('>H', crc16(bytes(frame)))

open('audio_flac_fixture.flac', 'wb').write(b'fLaC' + streaminfo + bytes(frame))
