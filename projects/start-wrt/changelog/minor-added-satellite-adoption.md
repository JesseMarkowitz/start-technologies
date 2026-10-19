- **A StartWRT router can serve as an access point of another.** On the Core, an
  Ethernet port can become a trunk that carries every Security Profile to access
  points cabled to it. A router in the satellite role with no Core yet answers the
  Core's announcement on that trunk; adopting it takes its sticker Wi-Fi password,
  or the adoption code a board without one shows, typed at the Core, and the
  password never crosses the cable. The access point then bridges each Wi-Fi
  password and each of its other ports onto the profile the Core assigns, with the
  Core handling addresses, DNS and policy for those devices exactly as for its own.
  Five wrong adoption attempts pause adoption for a minute on either side.
- **An access point that loses its Core goes dark, and the Core closes its port.** The
  Core passes profiles on a trunk port only while the access point behind it is in
  contact; an access point out of contact for about a minute takes its Wi-Fi down and
  cuts its other ports until the Core is back. An access point takes its time from the
  Core and reaches nothing beyond it except the Core's own update source. Removing an
  access point on the Core releases it to wait for a new Core.
- **Access points follow the Core's settings by themselves.** Every Wi-Fi password, profile
  and port assignment the Core holds reaches its access points within seconds of a change,
  with the Core choosing each access point's Wi-Fi channels. An access point that has been
  out of contact serves no one until it has caught up.
- **Access points are managed from the Core.** The new Access Points page adopts a waiting
  router, assigns its ports to profiles and removes it; the Ethernet page makes a port a
  trunk after a confirmation. Devices behind an access point show where they really are in
  the device list. An access point's own address offers only a status page pointing to the
  Core, and its settings cannot be changed there.
- **A dark access point can be reached and reset.** A minute after losing its Core, an access
  point opens a recovery network on its ports and, where it has a sticker password, as
  `StartWRT-Recovery-…` on Wi-Fi. There, `router.lan` says what it can tell about the fault
  and offers a restart and a factory reset. A fresh or reset router cabled to a Core waits to
  be adopted instead of becoming a main router, and a replaced access point can take over the
  saved configuration of the one it replaces.
