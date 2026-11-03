# Compatibility

Three different questions are easy to confuse. ROHI keeps them apart.

| Question | Answered by | Needs approval? |
|----------|-------------|-----------------|
| **Protocol compatibility**: can a ROHI client operate this firmware? | the firmware implements the [control protocol](../build/control-protocol.md) | no |
| **Publisher provenance**: who built and published this firmware? | the publisher's identity and the firmware's source | no, it is a fact about the firmware |
| **Physical hardware verification**: was it shown to work on this board? | a recorded hardware run: [support matrix](../support/overview.md) | no, it is evidence |

A firmware that implements the protocol is compatible. **Compatible custom firmware does not require central approval**:
nobody has to list, sign or accept it for `rohi` to operate it.

## Kinds of firmware

| Kind | Meaning | What to expect |
|------|---------|----------------|
| **Developer-verified** | built from this repository and verified by the ROHI developers on named hardware | claims in the [support matrix](../support/matrix.md) with `hw-report` evidence |
| **Community** | published by someone else for others to use | compatible by protocol; its support claims are the publisher's own, listed in the matrix only as `external` unless ROHI has assessed them |
| **Custom** | yours, built for yourself | compatible by protocol; nothing is claimed about it |

Today the only developer-verified claims are those in the matrix, and its hardware verification is mostly **not
recorded yet**: absence of a result means *not verified*, neither working nor broken. There is no certification program.

## What compatibility does not mean

- Compatible does not mean verified on your board: check the [support matrix](../support/matrix.md).
- Compatible does not mean secure: the control link is not paired or encrypted yet, see [Control protocol](../build/control-protocol.md#current-state).
- Compatible does not mean the same revision: the protocol is still under development, so keep client and firmware on one revision.
- Hardware being documented upstream does not mean ROHI supports it. See [Reading the support matrix](../support/overview.md).
