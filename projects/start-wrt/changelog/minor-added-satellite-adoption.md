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
