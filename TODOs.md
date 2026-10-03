# TODOs

Features for managing mirrored data across several USB-attached drives (each
data set lives on two drives — "twins" in the Drives tab).

## Mirror check

Pick two drives (or two indexed volumes, so one can be unplugged) and compare
them by relative path and content hash, bucketing every file as:

- **only on A / only on B**: the copies have drifted apart (a copy that never
  finished, something deleted on one drive only).
- **same path, different hash**: one copy is corrupted or was edited on one
  drive only. The most important bucket.
- **same hash, different path**: renamed or moved on one drive only; offer to
  apply the same rename to the other.
- **identical**: just a count.

The Drives tab already shows a content-only summary of this for twins
(`IndexDb::content_diff`); this would be the full per-file view.

## Bit-rot verification

Re-hash an attached drive and compare against the index. A file whose hash
changed while its modification time didn't is **silent corruption**. When it
happens, offer to restore it from the twin (after checking that the twin's
copy still matches the original hash). Record a "last verified" date per
drive and show it on its card next to "last indexed".

## Sync / repair between twins

Building on the mirror check: one- or two-way sync with a preview (like
Flatten): copy what's missing, restore corrupted files from the healthy copy,
carry renames across. Deletions go to a "pending" list rather than being
applied automatically, so a mistake doesn't spread to both copies.

## Single-copy report

Across the whole index, list files that exist on only **one** drive: the data
at risk. The opposite of duplicate finding: for backups, a duplicate is what
you want.

## Verified copies in Drive Fill

After copying, read each file back from the target and compare its hash,
instead of only hashing the source on the way.

## Act when a known drive is plugged in

The drive watcher (`drives::watch_drives`) already notices drives arriving.
Use that to offer a quick re-index, verification or health check of a tracked
drive that's overdue for one (optionally with a toast/notification).

## Exportable manifest

Write a `.b3sum`-style manifest to each drive's root, so a drive can be
verified without dupe-rs or its index.

## Smaller follow-ups

- **NVMe health**: SMART reading only speaks ATA (SAT pass-through or
  `SMART_RCV_DRIVE_DATA`). NVMe drives need the SMART / Health Information log
  page via `IOCTL_STORAGE_QUERY_PROPERTY` (`StorageDeviceProtocolSpecificProperty`).
- **Overall SMART verdict**: also issue SMART RETURN STATUS (needs `CK_COND` and
  parsing the ATA status return descriptor from the sense data), in addition
  to the threshold-based assessment.
- **Hardware serial as identity fallback**: a drive re-formatted with the same
  label gets a new volume serial and shows up as a new drive. Offer to merge it
  with the old record when the disk's hardware serial (from IDENTIFY) matches.
- **Elevated helper per check**: every health check shows a UAC prompt. If that
  gets annoying, a scheduled task or small service could read SMART without
  prompting.
