# Changelog

## [0.11.1](https://github.com/izz-linux/drovr/compare/v0.11.0...v0.11.1) (2026-11-12)


### Bug Fixes

* give the phone page drovr's icon on the Home Screen ([#50](https://github.com/izz-linux/drovr/issues/50)) ([7ad014e](https://github.com/izz-linux/drovr/commit/7ad014e728156f9063771541b030a1d7c1ab0e63))

## [0.11.0](https://github.com/izz-linux/drovr/compare/v0.10.3...v0.11.0) (2026-11-11)


### Features

* serve a phone page with push notifications and optional HTTPS ([#48](https://github.com/izz-linux/drovr/issues/48)) ([50749e3](https://github.com/izz-linux/drovr/commit/50749e37975b3f40f160c31adf6db2839f6949d5))

## [0.10.3](https://github.com/izz-linux/drovr/compare/v0.10.2...v0.10.3) (2026-11-06)


### Bug Fixes

* track herdr agent costs and runs ([#38](https://github.com/izz-linux/drovr/issues/38)) ([3fe1588](https://github.com/izz-linux/drovr/commit/3fe158851be48ddfc2bfebaafdd263515897ffb1))

## [0.10.2](https://github.com/izz-linux/drovr/compare/v0.10.1...v0.10.2) (2026-11-06)


### Bug Fixes

* focus an agent's live herdr pane instead of opening a second session ([#34](https://github.com/izz-linux/drovr/issues/34)) ([56011e9](https://github.com/izz-linux/drovr/commit/56011e98fe744d631057918b84d086446a207682))
* open herdr panes when drovr isn't started from one ([#33](https://github.com/izz-linux/drovr/issues/33)) ([75c720d](https://github.com/izz-linux/drovr/commit/75c720dffe6c9497d35114d69c9266ee90183246))
* start each herdr agent in its own tab and wait for its shell ([#37](https://github.com/izz-linux/drovr/issues/37)) ([78ab7e9](https://github.com/izz-linux/drovr/commit/78ab7e9bd18f6100f1c334de37fbde6461ebb18a))

## [0.10.1](https://github.com/izz-linux/drovr/compare/v0.10.0...v0.10.1) (2026-11-05)


### Bug Fixes

* keep report unread state when a folder differs by case or a file is gone ([#30](https://github.com/izz-linux/drovr/issues/30)) ([703e3c1](https://github.com/izz-linux/drovr/commit/703e3c188aa45b35ccec6d88aa39140c5bf8d1ff))
* preview pushes to a URL and gh pr create --repo ([#31](https://github.com/izz-linux/drovr/issues/31)) ([3a6acf3](https://github.com/izz-linux/drovr/commit/3a6acf34a32a3637a88eb47f225c09f09a490f7a))

## [0.10.0](https://github.com/izz-linux/drovr/compare/v0.9.1...v0.10.0) (2026-11-05)


### Features

* show a push's diff in the inbox ([#28](https://github.com/izz-linux/drovr/issues/28)) ([fcbc49d](https://github.com/izz-linux/drovr/commit/fcbc49d87e82a1ee23527368883fb0cf12574681))

## [0.9.1](https://github.com/izz-linux/drovr/compare/v0.9.0...v0.9.1) (2026-11-04)


### Bug Fixes

* share memory only from a folder you set in memory.dir ([#26](https://github.com/izz-linux/drovr/issues/26)) ([0c6e9d1](https://github.com/izz-linux/drovr/commit/0c6e9d1a3649305aff77da4726d056509ad77b77))

## [0.9.0](https://github.com/izz-linux/drovr/compare/v0.8.2...v0.9.0) (2026-11-04)


### Features

* share one Claude Code memory across agents, written by leads ([#24](https://github.com/izz-linux/drovr/issues/24)) ([ef9fbaa](https://github.com/izz-linux/drovr/commit/ef9fbaaa747aa0ee94a1ed093fd4a40489b3517f))
* show retro history in the picker and tell each retro what you decided before ([#23](https://github.com/izz-linux/drovr/issues/23)) ([ef007c7](https://github.com/izz-linux/drovr/commit/ef007c7058bb2666e987157e07b00b8db2a21a73))

## [0.8.2](https://github.com/izz-linux/drovr/compare/v0.8.1...v0.8.2) (2026-11-04)


### Bug Fixes

* run a retro's agent in its own folder when worktrees are on ([#21](https://github.com/izz-linux/drovr/issues/21)) ([f348731](https://github.com/izz-linux/drovr/commit/f348731a5b67b6df36e5d4808169ebbabb19cb45))

## [0.8.1](https://github.com/izz-linux/drovr/compare/v0.8.0...v0.8.1) (2026-11-04)


### Bug Fixes

* keep checking open PRs the GitHub search stops returning ([#19](https://github.com/izz-linux/drovr/issues/19)) ([8c62ca4](https://github.com/izz-linux/drovr/commit/8c62ca4ee915efea5b50ddd7c7baedf517dcd57e))

## [0.8.0](https://github.com/izz-linux/drovr/compare/v0.7.0...v0.8.0) (2026-11-03)


### Features

* agents query Datadog with pup, with read-only commands approved automatically ([106cb21](https://github.com/izz-linux/drovr/commit/106cb21e41be428745dc50601ab720f120a8442f))
* pick the model when starting an agent, a group's lead or a dispatched ticket ([106cb21](https://github.com/izz-linux/drovr/commit/106cb21e41be428745dc50601ab720f120a8442f))
* workers a lead starts without asking run on the lead's model, and the Spawn card takes a model ([106cb21](https://github.com/izz-linux/drovr/commit/106cb21e41be428745dc50601ab720f120a8442f))


### Bug Fixes

* republishing a report replaces every copy of it, whichever agent published it ([106cb21](https://github.com/izz-linux/drovr/commit/106cb21e41be428745dc50601ab720f120a8442f))
* the Inbox lists items oldest first, so new ones don't jump the queue ([106cb21](https://github.com/izz-linux/drovr/commit/106cb21e41be428745dc50601ab720f120a8442f))
* the Reports view remembers which folders are collapsed ([106cb21](https://github.com/izz-linux/drovr/commit/106cb21e41be428745dc50601ab720f120a8442f))

## [0.7.0](https://github.com/izz-linux/drovr/compare/v0.6.0...v0.7.0) (2026-10-31)


### Features

* show the copyright and license in About drovr, and link the repo from Help ([#11](https://github.com/izz-linux/drovr/issues/11)) ([050d09c](https://github.com/izz-linux/drovr/commit/050d09c7df71a400ed23d41690c53bdd5a5edfab))

## [0.6.0](https://github.com/izz-linux/drovr/compare/v0.5.0...v0.6.0) (2026-10-31)


### Features

* open the newest unread report when you go to Reports ([#7](https://github.com/izz-linux/drovr/issues/7)) ([916be9e](https://github.com/izz-linux/drovr/commit/916be9e32d6718d9e49d8a7d965b801bddad016a))


### Bug Fixes

* upgrade to SvelteKit 3 for its security fixes ([#5](https://github.com/izz-linux/drovr/issues/5)) ([be46fed](https://github.com/izz-linux/drovr/commit/be46fed14b4ed9d32bbef23f2ddfac113861b27d))

## Changelog
