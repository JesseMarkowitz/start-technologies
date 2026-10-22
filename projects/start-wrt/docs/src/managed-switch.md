# Connecting a Managed Switch

A managed switch adds Ethernet ports to your network while keeping each device on its own [Security Profile](security-profiles.md). The router sends every profile to the switch on one cable, and the switch decides which profile each of its ports belongs to.

## When You Need One

Each router port carries one profile, so a router with one LAN port gives all wired devices the same profile, and an ordinary (unmanaged) switch does not change that: everything behind it shares the profile of the port it hangs from. A managed switch is the way to have, for example, a StartOS server, a desktop and a TV on three different profiles when they all need a cable.

The switch must support **802.1Q VLANs** with per-port tagged and untagged membership and a per-port PVID. Inexpensive "smart" or "easy smart" switches do; unmanaged switches do not.

## How It Works

Setting a router port to **Managed switch** turns it into a trunk: the port carries every profile as a tagged VLAN, Admin included, and carries nothing untagged. On the switch, the port facing the router is configured the same way, and each device port is made an untagged member of exactly one profile's VLAN. A device plugged into the switch then gets its profile from the switch port it is on, exactly as it would from a router port, and appears in the [Devices](devices.md) list under that profile.

Profiles you create later are added to the trunk automatically. The switch, however, only knows about the VLANs you configured on it, so a new profile reaches a switch port only after you add its VLAN there.

> [!WARNING]
> Everywhere else, StartWRT decides which profile a port belongs to. On a managed-switch port it hands that decision to the switch's configuration and trusts it. Anyone who can log in to the switch can put any of its ports, including their own, on any profile. Protect the switch's admin password as you protect the router's, and finish the switch's VLAN setup before you connect it to the router.

Traffic that arrives on the trunk without a VLAN tag gets nothing: no address and no access. That is deliberate. A switch that is not yet configured, or a laptop plugged straight into the trunk cable, lands nowhere rather than in Admin.

## Setting It Up

1. Navigate to `Points of Entry > Ethernet` and set the port the switch will use to **Managed switch**. Read the confirmation and choose **Trust this switch**. Nothing is connected to the port yet, so this changes nothing for your devices.

1. Note the VLAN IDs now listed under that port, one per profile. Admin is always VLAN 1.

1. Configure the switch while it is **not** connected to the router, from a computer plugged directly into it (see the walkthrough below for one model).

1. Connect the switch's uplink port to that router port and plug your devices into the switch ports you prepared for them. Each should get an address from its profile's subnet and show up under that profile in [Devices](devices.md).

To undo it, set the port back to a profile. Devices on the switch then share that profile again, as behind an unmanaged switch.

> [!TIP]
> Give the switch's own management interface a static address in the Admin subnet and make the switch port you manage it from an Admin port. Leave unused switch ports in a "parking" VLAN that no device uses and that is not on the uplink, so a device plugged into the wrong jack gets nothing.

## Walkthrough: TP-Link ES208G

The ES208G is an eight-port 802.1Q switch with a web interface. The steps are the same on the rest of TP-Link's Easy Smart line, and other brands' VLAN pages ask for the same three things: VLAN membership per port, tagged or untagged, and a PVID per port. For the switch's own menus, see [TP-Link's user guide](https://www.tp-link.com/support/download/es208g/).

In this example port 1 is the uplink to the router, ports 2 to 4 are Admin, port 5 is the "Family" profile (VLAN 101), port 6 is the "Guests" profile (VLAN 102), and ports 7 and 8 are unused.

1. Plug a computer into port 2 and browse to the switch's address (`http://192.168.0.1` out of the box). Type `http://` explicitly: the switch has no HTTPS and some browsers upgrade the address on their own.

1. Under **System > IP Setting**, give the switch a static address that is free in your Admin subnet, so it does not collide with the router at `192.168.0.1`. Log back in at the new address.

1. Under **VLAN > 802.1Q VLAN**, enable 802.1Q. Then add each VLAN and set the membership of every port:

   | VLAN ID | Purpose        | Tagged | Untagged |
   | ------- | -------------- | ------ | -------- |
   | 1       | Admin          | 1      | 2, 3, 4  |
   | 101     | Family         | 1      | 5        |
   | 102     | Guests         | 1      | 6        |
   | 4000    | Uplink parking | –      | 1        |
   | 4001    | Unused ports   | –      | 7, 8     |

   Every port not listed for a VLAN is **Not member**. The uplink needs its own parking VLAN because this switch insists that every port is an untagged member of its PVID VLAN; a VLAN that holds only the uplink keeps untagged frames from reaching the router at all.

1. Under **VLAN > 802.1Q PVID Setting**, set each port's PVID to the VLAN it is an untagged member of: port 1 to 4000, ports 2 to 4 to 1, port 5 to 101, port 6 to 102, ports 7 and 8 to 4001. Set them **one port at a time**; selecting several ports at once applies the value to the wrong ones on this firmware.

1. Click **Save Config** in the menu, or the configuration is lost at the next power cycle. Back it up under **System > Backup and Restore** while you are there.

1. Connect port 1 of the switch to the router port you set to **Managed switch**.

Two habits of this firmware are worth knowing. It refuses to remove a port from the VLAN that is its PVID and silently flips it back to untagged, so when you move a port to another VLAN, add the new membership and change the PVID first, then remove the old membership. And the VLAN table is the truth: after any change, check it rather than the form you just submitted.

To add a profile later, create it in StartWRT first, then add its VLAN on the switch: tagged on the uplink and untagged on the ports that should carry it, with their PVID changed to match.
