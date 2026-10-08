#!/usr/bin/env python3
"""Measures a process's CPU, wake-ups and energy over an interval, from the kernel's own counters.

    python3 energy.py PID SECONDS

Prints one line, e.g.
    energy: 41.2% of one core, 3.1 W avg ... over 20.0 s

Reads proc_pid_rusage (RUSAGE_INFO_V6) at the start and end of the interval, so it needs no
root and adds no load. `energy` is the kernel's estimate of the CPU energy the process used
(ri_energy_nj; Apple Silicon), not the whole machine's. Wake-ups are what Activity Monitor's
"Idle Wake Ups" counts (package idle exits) plus interrupt wake-ups.
"""

import ctypes
import sys
import time

RUSAGE_INFO_V6 = 6

# struct rusage_info_v6 from <sys/resource.h>: 16-byte uuid, then uint64 fields in this order.
FIELDS = [
    "user_time", "system_time", "pkg_idle_wkups", "interrupt_wkups", "pageins", "wired_size",
    "resident_size", "phys_footprint", "proc_start_abstime", "proc_exit_abstime",
    "child_user_time", "child_system_time", "child_pkg_idle_wkups", "child_interrupt_wkups",
    "child_pageins", "child_elapsed_abstime", "diskio_bytesread", "diskio_byteswritten",
    "cpu_time_qos_default", "cpu_time_qos_maintenance", "cpu_time_qos_background",
    "cpu_time_qos_utility", "cpu_time_qos_legacy", "cpu_time_qos_user_initiated",
    "cpu_time_qos_user_interactive", "billed_system_time", "serviced_system_time",
    "logical_writes", "lifetime_max_phys_footprint", "instructions", "cycles", "billed_energy",
    "serviced_energy", "interval_max_phys_footprint", "runnable_time", "flags", "user_ptime",
    "system_ptime", "pinstructions", "pcycles", "energy_nj", "penergy_nj",
    "secure_time_in_system", "secure_ptime_in_system", "neural_footprint",
    "lifetime_max_neural_footprint", "interval_max_neural_footprint",
]


class RusageInfoV6(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(f, ctypes.c_uint64) for f in FIELDS] + [
        ("reserved", ctypes.c_uint64 * 9)
    ]


class MachTimebaseInfo(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
libproc = ctypes.CDLL("/usr/lib/libproc.dylib")


def ticks_to_seconds() -> float:
    """CPU times in rusage_info are mach absolute time units (24 MHz ticks on Apple Silicon)."""
    info = MachTimebaseInfo()
    libc.mach_timebase_info(ctypes.byref(info))
    return info.numer / info.denom / 1e9


def sample(pid: int) -> RusageInfoV6:
    info = RusageInfoV6()
    if libproc.proc_pid_rusage(pid, RUSAGE_INFO_V6, ctypes.byref(info)) != 0:
        raise OSError(ctypes.get_errno(), f"proc_pid_rusage({pid}) failed: is the process alive?")
    return info


def main() -> None:
    pid, seconds = int(sys.argv[1]), float(sys.argv[2])
    tick = ticks_to_seconds()
    before, started = sample(pid), time.monotonic()
    time.sleep(seconds)
    after, elapsed = sample(pid), time.monotonic() - started

    def delta(field: str) -> int:
        return getattr(after, field) - getattr(before, field)

    cpu = (delta("user_time") + delta("system_time")) * tick
    print(
        f"energy: {100 * cpu / elapsed:.1f}% of one core, "
        f"{delta('energy_nj') / elapsed / 1e6:.0f} mW avg, "
        f"{delta('pkg_idle_wkups') / elapsed:.0f} idle wake-ups/s, "
        f"{delta('interrupt_wkups') / elapsed:.0f} interrupt wake-ups/s, "
        f"footprint {after.phys_footprint / 2**20:.0f} MB, over {elapsed:.1f} s"
    )


if __name__ == "__main__":
    main()
