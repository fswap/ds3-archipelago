## 1.0.0-alpha.7

* Update to the latest upstream version of the static randomizer. This should
  reduce the number of weird in-game logic bugs and especially enemy randomizer
  bugs.

* Fix some cases where error messages would refer to DS3 instead of Sekiro.

* Increase the limit on log entries from 200 to 1000, and improve the way we
  handle large numbers of logs to avoid performance degradation.

## 1.0.0-alpha.6

* Use ModEngine3 version 0.11.0.

* Fix a bug where the static randomizer could break due to an inconsistency
  between its locations and those in the apworld.

## 1.0.0-alpha.5

* Send foreign items found in shops to the server rather than crashing.

## 1.0.0-alpha.4

* Don't grant items from other worlds over and over and over again.

## 1.0.0-alpha.3

* Put the `msg` directory in the correct location.

## 1.0.0-alpha.2

* Include the proper static randomizer data directory.

## 1.0.0-alpha.1

* Very early experimental release.
