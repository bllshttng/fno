"""A tiny notes index. The replica fold calls update() once per changed row."""


class IndexStore:
    """Row store with a derived index. Lookups must always see current text."""

    def __init__(self, seed_rows):
        self._rows = dict(seed_rows)
        self.rebuilds = 0

    def rebuild(self):
        # Stand-in for a full index rebuild over every row: the slow path
        # the sync snapshot still needs, because it replaces all rows at
        # once and a rebuild is the honest cost there.
        self.rebuilds += 1

    def update(self, note_id, text):
        self._rows[note_id] = text
        self.rebuild()

    def lookup(self, note_id):
        return self._rows.get(note_id)
