# Roadmap

What is planned, by release. The details, and the discussion, are in the linked issues; [milestones](https://github.com/sediros/sediros/milestones) show the progress. Plans change: a release ships what is ready and tested.

## 1.1 (released 2026-11-08)

- [x] `sediros-firstboot`: first-boot setup of cloud images from the provider's metadata, never running user-data scripts ([#1](https://github.com/sediros/sediros/issues/1))
- [x] Daily audit: reports when the hardening got worse, by e-mail, webhook or Prometheus ([#2](https://github.com/sediros/sediros/issues/2))
- [x] `sediros unlock`: unlock an encrypted server over SSH at boot ([#3](https://github.com/sediros/sediros/issues/3))
- [x] `sediros fleet`: the state of many servers in one table ([#4](https://github.com/sediros/sediros/issues/4))
- [x] Login greeting with the last audit result ([#7](https://github.com/sediros/sediros/issues/7))
- [x] `sediros pull` falls back to the SourceForge mirror when GitHub fails
- [x] Multipath TCP off (CVE-2026-98432 workaround)

## 1.2

- [ ] Maintenance window: reboot when an update needs it, off / ask / auto, safe with encrypted disks ([#9](https://github.com/sediros/sediros/issues/9))

## Later

- [ ] Windows build of the `sediros` CLI, for `sediros unlock` and `sediros fleet` ([#8](https://github.com/sediros/sediros/issues/8))
- [ ] Cloud images for VPS providers (Vultr, Hetzner), with `sediros-firstboot`
- [ ] Static IP address for unlocking the disk over SSH

## 2.0: Debian 14 (forky)

- [ ] `sediros-upgrade`: guided upgrade from SedirOS 1 to 2 ([#5](https://github.com/sediros/sediros/issues/5))

Ideas and requests are welcome as [issues](https://github.com/sediros/sediros/issues).
