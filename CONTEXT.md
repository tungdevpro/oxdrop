# Oxdrop

Oxdrop moves files in one direction: from a desktop computer (macOS or Windows) to an Android phone on the same local network.

## Language

### Roles

**Sender**:
The Oxdrop desktop application, running on macOS or Windows, that offers files.
_Avoid_: Desktop, host, server, source

**Receiver**:
The Android phone that takes delivery of files, using its web browser. There is no Oxdrop app on the phone.
_Avoid_: Phone, client, device, target

### What moves

**Transfer**:
A set of Items the Sender offers to the Receiver. A Transfer is open until the Sender ends it, the Sender quits, or it sits idle too long; once ended it cannot be reopened.
_Avoid_: Job, share, session, upload

**Item**:
One file or folder within a Transfer. A folder Item reaches the Receiver as a single archive that keeps its internal structure.
_Avoid_: Entry, attachment, payload

**Item Name**:
The name an Item is known by on both the Sender and the Receiver, unique within its Transfer (e.g. `photo.jpg`, `photo (2).jpg`).
_Avoid_: Filename, label, title

**Changed** / **Missing**:
The status of an Item whose files on disk no longer match, or no longer exist, as they were when the Item was added. Such an Item cannot be downloaded until the Sender refreshes it.
_Avoid_: Stale, broken, invalid

### Access

**Transfer Link**:
The address, shown as a QR code, that gives whoever holds it access to one open Transfer. Holding the Transfer Link is the only form of trust; there is no pairing.
_Avoid_: Pairing code, share link, session URL, token
