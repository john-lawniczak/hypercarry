# Dependency review — November 22, 2026

The refreshed advisory database invalidates the November 16 cached-clean result.
Mainnet release remains closed. The exact final lockfile must pass the dependency
policy before release. The implementation follow-up adds the narrowly bounded
maintenance exception described below; it does not claim chrono is maintained.

| Advisory | Locked dependency before review | Action / remaining status |
| --- | --- | --- |
| [RUSTSEC-2026-0486](https://rustsec.org/advisories/RUSTSEC-2026-0486) | rustix 1.1.4 | Upgrade to 1.1.6, the patched version. The advisory concerns zero-length extended-attribute buffers. |
| [RUSTSEC-2026-0592](https://rustsec.org/advisories/RUSTSEC-2026-0592) | url 2.5.8 | Upgrade to 2.5.9, which removes recursive origin handling for nested blob URLs. Resolution also updates form_urlencoded to 1.2.3. |
| [RUSTSEC-2026-0543](https://rustsec.org/advisories/RUSTSEC-2026-0543) | chrono 0.4.45 | Direct uses removed; exact storage-only maintenance exception through 2027-01-31. No patched version. This is a maintenance advisory, not a demonstrated exploit in Hypercarry. |

All direct workspace uses are removed. `time` 0.3.55 supplies UTC CLI rendering
and storage date partitions through `core::utc`; modern dates preserve the exact
three-digit RFC3339 format and daily partition names. Rendering outside time's
calendar range returns `None` and follows existing invalid-date handling. Tests
cover negative milliseconds, UTC midnight, leap day and out-of-range values.
`hypercarry_signing::ExpiryMs` keeps nonnegative integer milliseconds and the former
upper bound without a calendar conversion. Both signing domains retain their
official SDK vectors. No chrono dependency remains in the signing crate's graph.

Arrow/Parquet still requires chrono, including the checked upstream 60.2.0
manifests. Rather than introduce a storage fork during the signing migration,
the lockfile retains Arrow/Parquet 60.0.0 and chrono 0.4.45. `deny.toml` ignores only
the maintenance advisory, alongside the pre-existing paste maintenance exception.
`tools/check_dependency_policy.py`, also required in CI, rejects another chrono
version, direct workspace use, signing-crate dependency, any parent except
arrow-array/parquet 60.0.0, or use after **2027-01-31**. New vulnerability advisories
remain denied. This is a temporary maintenance-risk acceptance, not elimination
of the dependency; replacing or reassessing storage remains on TODO.md.

Run both checks together: `cargo deny check` and
`python3 tools/check_dependency_policy.py`. The refreshed audit and scope check
pass for this graph. Private working-tree receipts are implementation evidence;
retain fresh checks for the frozen candidate and review the exception in its
release decision. They do not authorize mainnet.
