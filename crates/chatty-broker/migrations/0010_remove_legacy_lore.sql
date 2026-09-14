-- World lore replaces the old global/conversation keyword store.
DROP TABLE lore;
DELETE FROM deltas WHERE entity_type='lore';
