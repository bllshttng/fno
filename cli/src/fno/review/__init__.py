"""The fno review lane's CLI-adjacent helpers.

The sigma panel this package once hosted is REMOVED : the bespoke
orchestrator, scorer, cache, runner and artifact surfaces are gone, and a
config still naming sigma is refused at init with the lane named as the
replacement. What remains is what the retained lanes use:

- the findings classifier, native in ``crates/fno-agents/src/review_findings.rs``
- :mod:`fno.review.cli` - ``fno do review invocations``
- :mod:`fno.review.invocation` - review invocation records
- :mod:`fno.review.locking` - the review lock the lane holds while it runs
"""
