# Walkthrough: TP-Link Easy Smart Switches

This walkthrough configures a TP-Link Easy Smart switch for a StartWRT [managed-switch port](managed-switch.md), page by page. It follows the example plan from that guide: port 1 is the uplink to the router, ports 2 to 4 are Admin, port 5 is the "Family" profile (VLAN 101), port 6 is the "Guests" profile (VLAN 102), and ports 7 and 8 are unused. Have your own plan beside you and substitute your ports and VLAN IDs.

It was written on an **ES208G**, hardware version 1.0, firmware 1.0.2. The other switches in TP-Link's Easy Smart and Easy Managed lines (ES205G, ES208G, TL-SG105E, TL-SG108E and their larger siblings) are sold with the same settings interface, and share the habits described at the end. For the switch's own menus beyond what is covered here, see [TP-Link's user guide](https://www.tp-link.com/support/download/es208g/) for your model.

> [!NOTE]
> TP-Link has shipped these switches with two different settings interfaces. The pages below are named for the classic one, which has the menu on the left. Newer hardware versions have an Omada-style interface with more pages; the section [On the Newer Interface](#on-the-newer-interface) tells you where each step lives there. Either way, check `System > System Info` first and note the hardware and firmware version with your plan.

## Before You Start

You need the switch, its power adapter, a laptop with an Ethernet port or adapter, and two Ethernet cables. Do everything below with the switch **not** connected to the router. The switch and the router both answer at `192.168.0.1` out of the box, and the switch has to be configured before the router can trust it in any case.

1. Give the laptop a fixed address for the setup. In its network settings, set the wired connection to a manual address of `192.168.0.5`, subnet mask `255.255.255.0`, no gateway. You will set it back to automatic at the end.

1. Plug the laptop into port 2 of the switch and power the switch on.

1. In a browser, go to `http://192.168.0.1`. Type the `http://` yourself: the switch has no HTTPS and some browsers quietly turn the address into one that fails. Log in as `admin` with the password from the printed guide in the box.

If the switch is not new, reset it first: hold the reset button on the back until the lights flash (the printed guide says how long), wait a minute, and log in as above.

## Address, Password and Cloud

1. Under `System > IP Setting`, turn DHCP off and set the address from your plan (the example uses `192.168.0.250`), the subnet mask `255.255.255.0` and the gateway `192.168.0.1`, then apply. The page stops responding because the switch has moved; go to `http://192.168.0.250` and log in again.

1. Under `System > User Account`, set a new password. Record it with your plan.

1. If the switch offers cloud or controller management, turn it off: on the classic interface it is a cloud or controller item in the menu; on the newer interface it is `Controller Settings`, where **Cloud-Based Controller Management** goes off and the **Inform URL** stays empty.

## VLANs

1. Under `VLAN > 802.1Q VLAN`, set **802.1Q VLAN Configuration** to Enable and apply. The table below the form now shows VLAN 1 with every port untagged.

1. Create each VLAN in your plan **except VLAN 1**, one at a time: enter the VLAN ID and a name, set each port to **Tagged**, **Untagged** or **Not Member**, and apply. For the example:

   | VLAN ID | Name   | Tagged | Untagged | Not Member       |
   | ------- | ------ | ------ | -------- | ---------------- |
   | 101     | Family | 1      | 5        | 2, 3, 4, 6, 7, 8 |
   | 102     | Guests | 1      | 6        | 2, 3, 4, 5, 7, 8 |
   | 4000    | uplink | –      | 1        | 2 to 8           |
   | 4001    | unused | –      | 7, 8     | 1 to 6           |

   After each one, find it in the table and check the **Tagged Ports** and **Untagged Ports** columns against your plan. The table is what the switch will do; the form is only what you asked for.

1. Under `VLAN > 802.1Q PVID Setting`, set the PVID of **one port at a time**: select only that port, enter the VLAN ID, apply, and read the table. Port 1 to 4000, ports 2, 3 and 4 to 1, port 5 to 101, port 6 to 102, ports 7 and 8 to 4001. Selecting several ports at once applies the value to the wrong ports on this firmware. Leave **Ingress Checking** enabled.

1. Back under `VLAN > 802.1Q VLAN`, edit VLAN 1: enter VLAN ID 1 again, set port 1 to **Tagged**, ports 2, 3 and 4 to **Untagged**, and ports 5 to 8 to **Not Member**, and apply. Entering an existing VLAN ID replaces its membership. Check the table: VLAN 1 should now list port 1 as tagged and ports 2, 3 and 4 as untagged, and nothing else.

Your laptop is on port 2, which is untagged in VLAN 1 throughout, so the settings page stays open through all of this.

## Ports, Loops and Saving

1. Under `Switching > Port Setting`, set the **State** of ports 7 and 8 to Disabled, one at a time, and apply. The **Actual** column shows Disabled for any port with nothing plugged in, which is normal and not a sign of a problem.

1. Under `Monitoring > Loop Prevention`, confirm it is Enabled. It usually is on the classic interface and usually is not on the newer one, where it is called `Loop Detection` and has a per-port table to enable as well.

1. Click **Save Config** in the menu and confirm. Without this, the switch forgets everything at the next power cut.

1. Under `System > Backup and Restore`, download the backup and keep the file with your important files. It contains the switch's address and the admin password in readable form, so do not share it or put it anywhere public.

1. Set the laptop's wired connection back to automatic.

The switch is ready. Return to the guide for [testing with a laptop](managed-switch.md#testing-with-a-laptop) once the uplink is connected to the router.

## Habits of This Firmware

These are the ways the switch will undo or refuse a change without telling you. Each is the reason for the order above.

- **A port cannot leave the VLAN that is its PVID.** The switch refuses the removal and silently puts the port back as untagged. To move a port to another VLAN, add the new membership, change the PVID, and only then remove the old membership.

- **Deleting a VLAN that is some port's PVID resets that port's PVID to 1**, with no warning, which makes it an Admin port. Move the PVID to another VLAN before deleting, and read the PVID table after any delete.

- **A port removed from every VLAN is put back in VLAN 1**, and VLAN 1 cannot be deleted. A port can leave VLAN 1 once it belongs somewhere else, which is why the other VLANs are created first.

- **The PVID page applies a value to the wrong ports** when several are selected. One port per apply.

- **Entering an existing VLAN ID on the VLAN page replaces that VLAN's membership** rather than adding to it. This is how you edit a VLAN, and also how you accidentally wipe one.

- **Changes live in memory until Save Config.** A power cycle before saving restores the old configuration, which is also a way out of a mistake.

- **The backup file is readable and tied to the hardware version.** Never load one switch's backup into a different unit.

- **The settings page answers on every VLAN.** The switch's admin page can be reached by a computer on any port that gives itself an address in the Admin subnet, whatever VLAN the port is on. The router's firewall keeps other profiles' devices from reaching the Admin subnet normally, but the password is what guards the switch. Change it.

## On the Newer Interface

Later hardware versions have an Omada-style interface. These locations are taken from TP-Link's Omada Agile (Easy Managed) Switch user guide and have not yet been checked on a unit; if yours differs, the page names in the left menu are close enough to find.

| Step                         | Classic interface                                 | Newer interface                                                                |
| ---------------------------- | ------------------------------------------------- | ------------------------------------------------------------------------------ |
| Hardware and firmware        | `System > System Info`                            | `System > System Info`                                                         |
| Switch address               | `System > IP Setting`                             | `System > IP Setting`                                                          |
| Password                     | `System > User Account`                           | `System > User Account`                                                        |
| Cloud management off         | cloud or controller item                          | `Controller Settings`                                                          |
| Turn on 802.1Q, create VLANs | `VLAN > 802.1Q VLAN`                              | `VLAN > 802.1Q VLAN > VLAN Config`                                             |
| PVID and ingress checking    | `VLAN > 802.1Q PVID Setting`                      | `VLAN > 802.1Q VLAN > Port Config`                                             |
| Management VLAN              | none                                              | `VLAN > 802.1Q VLAN > Management VLAN`: leave it at 1, which is Admin          |
| Disable ports                | `Switching > Port Setting`                        | `Switching > Port Settings`                                                    |
| Loop protection              | `Monitoring > Loop Prevention`                    | `Monitoring > Loop Detection`, off by default: enable it globally and per port |
| Save and back up             | `Save Config`, then `System > Backup and Restore` | `System Tools > Backup Restore`                                                |
| Factory reset                | `System > System Tools > Reset`                   | `System Tools > Reset`                                                         |
