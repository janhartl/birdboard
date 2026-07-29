# BirdBoard

*NBA fantasy draft assistant TUI built with Rust*

```text
                \
            \\  \
        \\\\   \
        \\\\\\    \

        B I R D B O A R D
```

*"Who's playing for 2nd?"*

---
## Architecture

### Preparation Data

Information known before the auction starts.

- Player rankings and projected values from `players.csv`
- Player positions
- Category projections
- Tiers, notes, targets, and avoid lists
- Fantasy team names
- Starting budget
- League categories and roster settings
- Nomination order

### Live Auction State

Facts describing what has actually happened during the auction.

- Whether the auction has started
- Current nominating team
- Draft picks:
  - player
  - winning team
  - purchase price
- Current nomination, when applicable
- ESPN synchronization status
- Manual corrections or undone picks

### Derived Analysis

Information recalculated from preparation data and live auction facts.

- Available and drafted players
- Remaining team budgets
- Team rosters
- Auction inflation or deflation
- Category strengths and weaknesses
- Positional and category needs
- Player scarcity and replacement value
- Best available players
- Suggested targets and maximum bid prices

### UI State

Information needed only to control what the user currently sees and does.

- Current screen
- Selected player
- Selected fantasy team
- Big-board scroll position
- Active filters and sorting
- Draft interaction mode:
  - browsing players
  - choosing a team
  - entering a price
- Temporary price input
- Status and error messages
