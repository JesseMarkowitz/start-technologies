# Connecting a Managed Switch

A managed switch adds Ethernet ports to your network while keeping each device on its own [Security Profile](security-profiles.md). The router sends every profile to the switch over one cable, and the switch decides which profile each of its ports belongs to. This guide takes you from choosing a switch to a working, tested setup, and is written for someone who has never configured one before.

## When You Need One

Each router port carries one profile. Plugging an ordinary switch into a router port does not change that: every device behind it shares the profile of the port it hangs from. If you need, for example, a StartOS server, a desktop and a TV on three different profiles and all three need a cable, a managed switch is the way to do it. One cable runs from the router to the switch, and each switch port carries the profile you choose for it.

If every wired device in a room can share one profile, an ordinary switch is simpler and there is nothing to configure.

## How It Works

Inside the router, each profile is a separate numbered lane called a **VLAN**. On a normal router port the lane is invisible: the port belongs to one profile, so there is nothing to tell apart. On a port set to **Managed switch**, the router sends all the lanes down one cable at once, and marks each packet with its lane number so the switch can sort them. A packet with a lane number is **tagged**; a packet without one is **untagged**.

The switch does the sorting. Its port facing the router, the **uplink**, is told to expect tagged packets for every profile. Each of its other ports is told to belong to one profile: packets arriving from the device on that port get that profile's lane number, and packets leaving to the device have the number removed, so the device sees ordinary, untagged traffic and never knows the lanes exist. Four words cover everything a switch's VLAN page will ask you:

| Word         | What it means on a switch port                                                                                          |
| ------------ | ----------------------------------------------------------------------------------------------------------------------- |
| **VLAN**     | A numbered lane. In StartWRT, each profile has one. Admin is always VLAN 1.                                             |
| **Tagged**   | The port sends and expects packets carrying the lane number. Used only on the cable to the router or to another switch. |
| **Untagged** | The port sends and expects plain packets, with the lane number added or removed by the switch. Used for devices.        |
| **PVID**     | The lane a port puts a plain, incoming packet into. For a device port, the same VLAN the port is untagged in.           |

A device plugged into the switch gets its profile from the switch port it is on, exactly as it would from a router port, and appears in the [Devices](devices.md) list under that profile. Profiles you create later are added to the router's side of the cable automatically. The switch only knows about the VLANs you configured on it, so a new profile reaches a switch port only after you add its VLAN on the switch.

## What Can Go Wrong, and What Protects You

Everywhere else, StartWRT decides which profile a port belongs to. On a managed-switch port it hands that decision to the switch and trusts it. Before you buy anything, know the five ways this can go wrong and what stands in the way of each.

- **The switch is not configured yet, or a laptop is plugged straight into the trunk cable.** Nothing bad happens. Traffic that reaches the router without a lane number gets no address and no access, so an unconfigured switch, a wrong cable or a factory-reset switch lands its devices nowhere rather than in Admin. This is deliberate, and it means a mistake on the switch shows up as "no connection", never as "the wrong profile".

- **A switch port is set to the wrong profile.** The device on it joins that profile. The [Devices](devices.md) page shows every device under the profile it actually landed in, so this is caught by looking, and fixed on the switch. The laptop test in this guide catches it before any real device is connected.

- **Someone logs in to the switch.** This is the one thing the router cannot protect. Anyone who can sign in to the switch's admin page can put any of its ports, including the one they are plugged into, on any profile, Admin included. Protect the switch's password as you protect the router's, change it from the factory default before the switch touches the router, and keep the switch somewhere a guest cannot reach a cable. Inexpensive switches typically serve their admin page over plain HTTP, and the one in the walkthrough answers on every VLAN, so the password is the only gate.

- **Two cables between the same two boxes.** A second cable between the switch and the router, or between two switches, is a loop, and a loop can bring the whole network to a crawl within seconds. Run exactly one cable between any two boxes, turn on the switch's loop protection, and disable the switch ports you are not using so a stray cable does nothing.

- **The switch forgets its settings.** Many switches keep changes in memory only until you click a save button, and all of them lose their settings on a factory reset. Every device behind a switch that has forgotten its VLANs goes dark, as above. Save the configuration, download a backup, and keep it with your other important files.

## Choosing a Switch

The switch must support **802.1Q VLANs** with, for each port, tagged or untagged membership in a VLAN and a PVID. Those exact terms appear on the box or the product page. Switches sold as "smart", "easy smart", "easy managed" or "web managed" have them; "unmanaged" switches have no settings at all and will not do. A fully managed, enterprise-style switch also works but costs more and asks more questions.

Also look for:

- **A local web page for configuration**, so you are not required to create a cloud account. If the switch offers cloud or app management, you will turn it off.
- **Loop protection** (also called loop detection or loop prevention).
- **A way to disable individual ports.**
- **Configuration backup** to a file.
- **Enough ports.** Count the devices for the room, add one for the cable to the router, and leave a spare.

The following models have been set up and tested with StartWRT by following the walkthrough linked:

| Switch                                           | Hardware             | Firmware             | Walkthrough                                              |
| ------------------------------------------------ | -------------------- | -------------------- | -------------------------------------------------------- |
| TP-Link ES208G (8-port, Easy Smart/Easy Managed) | Hardware version 1.0 | 1.0.2 Build 20250609 | [TP-Link Easy Smart Switches](managed-switch-tp-link.md) |

Other brands ask for the same three things on their VLAN pages: which VLANs a port is in, tagged or untagged, and each port's PVID. The names of the pages differ, and the walkthrough's closing section tells you what to look for.

## The Router Side

1. Navigate to `Points of Entry > Ethernet` and set the port the switch will use to **Managed switch**. Read the confirmation and choose **Trust this switch**. Nothing is connected to the port yet, so this changes nothing for your devices.

1. The port now lists each profile with its VLAN ID. Admin is always VLAN 1. These numbers go into your plan in the next section.

Leave the cable to the switch unplugged until the switch is configured.

## Planning Your Ports

Decide everything on paper before you open the switch's settings. The example used through the rest of this guide is a router with three profiles and an eight-port switch in a living room, with a desktop and a laptop on Admin, a StartOS server on Admin, a games console on a "Family" profile and a smart TV on a "Guests" profile.

1. Navigate to `Points of Entry > Ethernet` and write down the VLAN ID listed under the managed-switch port for each profile:

   | Profile | VLAN ID |
   | ------- | ------- |
   | Admin   | 1       |
   | Family  | 101     |
   | Guests  | 102     |

1. Give every switch port a job. Each port is either the **uplink** (the one cable to the router), a **device port** on exactly one profile, or **unused**:

   | Port | Job                  | Device         |
   | ---- | -------------------- | -------------- |
   | 1    | uplink to the router |                |
   | 2    | Admin                | desktop        |
   | 3    | Admin                | laptop         |
   | 4    | Admin                | StartOS server |
   | 5    | Family               | games console  |
   | 6    | Guests               | smart TV       |
   | 7    | unused               |                |
   | 8    | unused               |                |

   Keep at least one Admin port, even if nothing lives on it permanently. It is where you plug in to manage the switch later.

1. Add two **parking** VLANs that no profile uses. These exist because many switches insist that every port is untagged in some VLAN; a VLAN the router does not carry makes that harmless. In the example, VLAN **4000** holds only the uplink and VLAN **4001** holds the unused ports. StartWRT numbers profile VLANs upward from 101, so choose high numbers that will never collide.

1. Choose an address for the switch's own settings page. It must be in the Admin subnet, `192.168.0.x` unless you changed the [LAN network block](lan.md), and it must not be one the router gives out. The router hands out `.2` to `.199` by DHCP and keeps `.200` to `.254` for the Admin profile's [Inbound VPN](inbound-vpn.md) clients, so pick an address from `.200` to `.253` that no VPN client on the Admin profile uses, and skip it when you add VPN clients later. The example uses `192.168.0.250`. The gateway is the router's Admin address, `192.168.0.1`.

Write this plan down. You will check the switch against it twice: once on the switch's own VLAN page and once with a laptop.

## The Switch Side

Do all of this with the switch **not** connected to the router, from a laptop plugged into the port you planned as Admin. The order matters on most switches, because they refuse some changes in other orders or silently undo them. The walkthrough for your model gives the page names; the steps are the same everywhere.

1. **Factory-reset the switch** unless it is new in the box, so it starts from a known state: every port untagged in VLAN 1.

1. **Set the switch's address, password and cloud settings first.** Give it the static address from your plan, with the router's Admin address as its gateway, and turn off any DHCP option so the switch keeps the address. Change the admin password. Turn off cloud or app management. Log back in at the new address.

1. **Turn on 802.1Q VLANs.**

1. **Create every VLAN from your plan, with its full port membership.** For each profile VLAN, the uplink is **tagged** and the device ports for that profile are **untagged**; every other port is not a member. The uplink parking VLAN has only the uplink, untagged. The unused parking VLAN has only the unused ports, untagged. In the example:

   | VLAN ID | Purpose        | Tagged | Untagged |
   | ------- | -------------- | ------ | -------- |
   | 1       | Admin          | 1      | 2, 3, 4  |
   | 101     | Family         | 1      | 5        |
   | 102     | Guests         | 1      | 6        |
   | 4000    | Uplink parking | –      | 1        |
   | 4001    | Unused ports   | –      | 7, 8     |

   Create the parking VLANs and the non-Admin profile VLANs before you touch VLAN 1. A port usually has to belong somewhere else before the switch lets it leave VLAN 1.

1. **Set each port's PVID** to the VLAN it is untagged in: port 1 to 4000, ports 2 to 4 to 1, port 5 to 101, port 6 to 102, ports 7 and 8 to 4001. Do this one port at a time, and read the table back afterwards.

1. **Take every port out of VLAN 1 except the uplink and the Admin device ports.** Switches put every port in VLAN 1 to begin with, and a port left untagged in VLAN 1 would be an Admin port. In the example, VLAN 1 ends up with port 1 tagged and ports 2, 3 and 4 untagged, nothing else. Read the VLAN table back and compare it to your plan line by line. The table is the truth, not the form you just submitted.

1. **Disable the unused ports.** Never disable the port your laptop is on.

1. **Turn on loop protection.**

1. **Save the configuration** if your switch has a separate save step, and **download a backup file**. Keep the backup with your important files; on many switches it contains the admin password, so do not share it.

When you are done, every switch port satisfies three rules. The uplink is tagged in every profile VLAN and untagged only in its parking VLAN. Every device port is untagged in exactly one profile VLAN, with its PVID set to that VLAN, and is in no other. Every unused port is untagged only in the unused parking VLAN and is disabled.

## Testing With a Laptop

Connect the switch's uplink port to the router port you set to **Managed switch**. Before any real device goes on the switch, try each kind of port with a laptop on automatic network settings:

| Laptop plugged into     | Expected result                                                                                                 |
| ----------------------- | --------------------------------------------------------------------------------------------------------------- |
| An Admin port           | An address starting `192.168.0.`, and the laptop appears under Admin on the [Devices](devices.md) page.         |
| A Family or Guests port | An address in that profile's subnet, the laptop under that profile on the Devices page, and the Internet works. |
| An unused port          | No address.                                                                                                     |
| The uplink port         | No address. The cable to the router is the only thing that belongs here.                                        |

The last two rows look like failures and are the point of the test: a port that gives a laptop nothing cannot give a stranger's device anything either. Also open the switch's settings page from the Admin port, at its new address, to confirm the address and gateway are right.

If every row matches, the switch is right. If a row is wrong, see [If Something Is Wrong](#if-something-is-wrong) before plugging in anything else.

## Connecting Your Devices

Plug each device into the port you planned for it. Each gets an address from its profile's subnet and shows up under that profile on the [Devices](devices.md) page, where you can name it and reserve its address like any other device.

Everything a profile allows applies to the device as usual: its Internet access, its outbound VPN, its blackout schedule and which other profiles it can reach. Casting, printing and other "find devices nearby" features work between devices on the same profile, on the switch or anywhere else.

## Making Changes Later

- **Adding a profile.** Create it in StartWRT first; the router adds it to the managed-switch port by itself. Then add its VLAN on the switch: tagged on the uplink, untagged on the ports that should carry it, with their PVID changed to match. Until you do, devices on those ports keep their old profile, and the new profile has no port on the switch.

- **Moving a device to another profile.** Move it to a port on that profile, or change the port's VLAN and PVID on the switch. Either way the device drops its old address and gets a new one; anything that pointed at the old address, such as a [published port](published-ports.md), needs updating.

- **Using a parked port.** Give it a profile VLAN and PVID as in steps 4 and 5 of [The Switch Side](#the-switch-side), take it out of the unused parking VLAN, and enable it.

- **Adding a second switch.** A switch can feed another switch. On the first switch, configure one port exactly like its uplink: tagged in every profile VLAN, untagged only in the uplink parking VLAN, with that VLAN as its PVID. On the second switch, configure its uplink the same way and its device ports as usual, then connect the two with one cable. Every switch between the router and a device has to carry every profile VLAN, so when you add a profile, add it on every switch. Keep exactly one cable between any two switches.

## If Something Is Wrong

| What you see                                            | Likely cause                                                                                           | What to do                                                                                                                          |
| ------------------------------------------------------- | ------------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------- |
| A device on the switch gets no address                  | Its port's VLAN is not tagged on the uplink, or the port's PVID is not the VLAN it is untagged in      | Compare the switch's VLAN and PVID tables to your plan. Check the router port is still set to **Managed switch**.                   |
| Every device on the switch lost its connection at once  | The switch lost its settings, often a power cycle before saving                                        | Restore the backup file, or reconfigure from your plan. Save, then back up again.                                                   |
| A device is under the wrong profile on the Devices page | Its switch port is untagged in the wrong VLAN, or still in VLAN 1                                      | Fix the port on the switch. A port untagged in VLAN 1 is an Admin port.                                                             |
| A new profile does not work on the switch               | Its VLAN was never added on the switch                                                                 | Add it: tagged on the uplink, untagged on its device ports, PVIDs to match.                                                         |
| Small things work but downloads and page loads stall    | A switch along the path is not passing full-size tagged packets                                        | Check for a "jumbo frame" or "frame size" setting on the switch; every 802.1Q switch should pass 1522-byte frames.                  |
| The switch's settings page cannot be opened             | Your computer is on a profile that cannot reach Admin, or the switch's address is not what you planned | Plug into an Admin port. If the address is wrong, give your computer a fixed Admin-subnet address and look for the switch that way. |
| The network crawls or stops after plugging in a cable   | A loop: two cables between the same boxes, or a cable from one switch port to another                  | Unplug the last cable you connected. Turn on loop protection and disable unused ports.                                              |

## Undoing It

Set the router port back to a profile under `Points of Entry > Ethernet`. Every device on the switch then shares that profile, as it would behind an ordinary switch, and the switch's VLAN settings are ignored. You can leave the switch configured in case you want to come back.

To return the switch itself to factory settings, hold its reset button or use the reset option on its settings page. A factory-reset switch behind a **Managed switch** port gives its devices nothing until it is configured again.
