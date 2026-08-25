---
name: Bug report
about: Something in the compiler, UI engine, or docs is wrong
title: ""
labels: bug
assignees: ""
---

**What happened**

A clear description of what went wrong.

**Diagnostic output**

Run the failing case with `nirdosha <file.nir> --format=json` and paste
the `Diagnostic` JSON here — it's the fastest way to pin down exactly
where things went wrong.

```json
paste here
```

**Minimal repro**

The smallest `.nir` snippet that reproduces it, if the diagnostic alone
doesn't make the bug obvious.

```nirdosha
paste here
```

**Expected vs. actual**

What you expected to happen, and what actually happened instead.

**Environment**

- Nirdosha version/commit:
- OS:
- Built from source or prebuilt binary:
