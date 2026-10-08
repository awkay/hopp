#!/usr/bin/env python3
"""Splits a profile's CPU by audit finding.

    buckets.py viewer.folded.gz 0.28

The second argument is core's average CPU in cores (0.28 = 28% of one core, from viewer.txt), to
turn shares into "% of one core". Each stack goes to the first rule with a pattern in any of its
frames, so the order of RULES matters.
"""

import gzip
import sys
from collections import defaultdict

RULES = [
    ("Noise filter (DTLN, tract) [perf #7, alloc]", ["tract_core", "tract_linalg", "denoiser"]),
    ("Echo cancel / APM [perf #7]", ["AudioProcessingModule", "webrtc::aec3", "rnn_vad", "EchoCanceller"]),
    ("Mic: Opus encode + send", ["ChannelSend::ProcessAndEncodeAudio", "AudioCodingModuleImpl"]),
    ("Remote audio: decode + mix + playout [perf #8]", ["NeedMorePlayData", "AudioMixerImpl", "hopp_core::audio::mixer", "AudioDeviceIOProc", "com.apple.audio", "acm2::", "NetEq", "opus_decode"]),
    ("Mic capture (rodio/cpal) [perf L1]", ["hopp_core::audio::capturer", "cpal::", "rodio::"]),
    ("Black-frame keepalive encode [new]", ["VideoStreamEncoder", "FrameCadenceAdapter", "RTCVideoEncoderH264", "VTCompressionSession"]),
    ("Viewer: to_i420 + copy_from_i420 [perf #3, alloc #2]", ["process_video_stream", "copy_from_i420"]),
    ("Viewer: YUV upload in prepare, main thread [perf #3]", ["YuvVideoPrimitive"]),
    ("Video receive + decode (libwebrtc, VideoToolbox)", ["VideoReceiveStream2", "VTDecompression", "RTCVideoDecoderH264", "VideoRtpDepacketizer", "RtpVideoStreamReceiver", "H264::"]),
    ("Rendering: iced/wgpu/Metal, all windows [perf #4]", ["wgpu", "iced", "MTL", "Metal", "AGX", "IOGPU", "CAMetal", "FPCAMetalLayer", "CA::", "QuartzCore", "winit"]),
    ("libwebrtc network thread (sockets, poll, SRTP)", ["network_thread"]),
    ("libwebrtc worker/signaling threads", ["worker_thread", "signaling_thread"]),
    ("Stats loop [lock #2]", ["stats_loop", "collect_stats", "GetStats"]),
]


def read_folded(path):
    opener = gzip.open if path.endswith(".gz") else open
    with opener(path, "rt") as lines:
        for line in lines:
            stack, weight = line.rsplit(" ", 1)
            yield stack, int(weight)


def main():
    path, cores = sys.argv[1], float(sys.argv[2])
    total = 0
    by_rule = defaultdict(int)
    for stack, weight in read_folded(path):
        total += weight
        rule = next((name for name, patterns in RULES if any(p in stack for p in patterns)), "Other")
        by_rule[rule] += weight
    print(f"core total: {cores * 100:.0f}% of one core")
    for rule, weight in sorted(by_rule.items(), key=lambda item: -item[1]):
        print(f"  {100 * weight / total:5.1f}% of core = {cores * 100 * weight / total:4.1f}% of one core   {rule}")


if __name__ == "__main__":
    main()
