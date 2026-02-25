## 4.0.0-rc.4

* Enable the DLC by default.

* Don't allow a player to disable DLC and still require that they beat a DLC
  boss.

* Fix the "Painted World of Ariandel End Boss" victory condition.

* Fix some cases where item smoothing could cause invalid item placements. 

## 4.0.0-rc.3

* Fix a bug where logs were being displayed in reverse order in the overlay.

* Fix a bug where, if you load into a save with the overlay collapsed, it won't
  be openable afterwards.

* Ensure that the required Archielago version in the DS3 options template is
  accurate.

## 4.0.0-rc.2

* Include the connection between Kiln of the First Flame and Dreg Heap in logic
  as long as the Kiln of the First Flame boss isn't the only goal.

* Limit the log history to 200 messages long to avoid slowdown in long sessions.

* Fix a bug where the static randomizer could crash if Yhorm was randomly placed
  in his vanilla location.

* Fix another case where the config file's location could be detected
  incorrectly on Linux.

## 4.0.0-rc.1

### Features

* Provide an in-game overlay which shows the Archipelago connection status and
  messages.

* Provide a pre-assembled Linux release.

* Create a separate DS3 save file for each Archipelago seed.

* When viewing an Archipelago item in a shop, the client will now post a hint to
  the server with the item's location.

#### Options

* Auto-equip is no longer supported.

* Add an option to customize the Archipelago goal. This only works with the
  bundled apworld. See the documentation of `goal` in `Dark Souls III Options
  Template.yaml` for details.

* Add an option to only send death links when you die without picking up your
  bloodstain.

* Add an option to customize death link amnesty. See the documentation of
  `death_link_amnesty` in `Dark Souls III Options Template.yaml` for details.

### Game Logic

* Don't make the Firelink Set locations available upon receiving Soul of the
  Lords.

### Locations

* Add `Drops` and `Shops` location groups.

* Change the descriptions of the Firelink Set to more accurately say "shop after
  beating KFF boss" instead of "shop after placing all Cinders".

* Consider the Firelink Set locations to be logically behind the KFF boss.

* Mark `FS: Titanite Slab - shop after placing all Cinders` as a shop item
  and not a hidden item.

* Fix a bug where rings would be placed in the incorrect locations.

* Fix a bug where stacks of items could be placed in a few of Yuria's shop
  locations.
  
### Static Randomizer

* Fix a bug that was causing "object reference not set to an instance of an
  object" errors.

* Improve the static randomizer feedback for invalid enemy presets.
