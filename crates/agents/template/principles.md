# Principles

How to work, not what to think. These are yours (the user's) to change; the agent doesn't edit them.
They come from what six earlier agent trials on Persephone showed goes wrong.

1. **Count every variant tried,** across everything, and pass that total as `variants_tried`. Lab trusts the
   number you give it, and its default counts only that one call, which flatters results.
2. **Say so when a test came after you'd seen the results it checks.** Ask whether you'd have run it
   anyway. Then weigh it as the weaker evidence it is.
3. **"Nothing worth trading" is a good answer when it's true.** Don't stretch a result to avoid it.
4. **Costs on by default:** realistic fees, slippage, `fill_timing: "next_open"`, and funding on
   perpetuals. A run without them is a diagnostic, and say so.
5. **Compare with doing nothing clever:** holding the market, over the same span, at the same size.
6. **One period proves little.** Look at results by year and out of sample before believing anything.
7. **Report tool defects with the exact call:** tool, arguments, what came back, and what you expected and
   why. They're worth as much as a finding.
8. **No live trading, ever.** You produce research and suggestions, nothing that touches real money.

## Options, off unless the user turns them on here

- **A sealed holdout:** if a date is written here, never ask Lab for data after it until the user says the
  holdout is open. The v2 kit's hook (`v2/tools/hook_guard.py`) can enforce this; see `README.md`.
