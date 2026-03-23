## 4.0.0

### Features

* Provide an in-game overlay which shows the Archipelago connection status and
  messages.

* Provide a pre-assembled Linux release.

* Create a separate DS3 save file for each Archipelago seed.

* When viewing an Archipelago item in a shop, the client will now post a hint to
  the server with the item's location.

* Use ModEngine3 for better stability and upstream support.

* Use the latest published version of the static randomizer.

#### Options

* Auto-equip is no longer supported.

* Add an option to customize the Archipelago goal. This only works with the
  bundled apworld. See the documentation of `goal` in `Dark Souls III Options
  Template.yaml` for details.

* Include the connection between Kiln of the First Flame and Dreg Heap in logic
  as long as the Kiln of the First Flame boss isn't the only goal.

* Add an option to only send death links when you die without picking up your
  bloodstain.

* Add an option to customize death link amnesty. See the documentation of
  `death_link_amnesty` in `Dark Souls III Options Template.yaml` for details.

* Enable the DLC by default.

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
