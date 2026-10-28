---
user-facing: false
---

- Creating a profile with an explicit VLAN ID now rejects 0, 4095 and above, and any ID another profile already holds, Admin's included. The duplicate check read only the bridge VLAN sections, so on a router with no tagged ports Admin's VLAN had none and could be claimed. The web interface never offers a VLAN ID; the RPC does.
