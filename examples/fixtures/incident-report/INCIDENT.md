# Incident 2291: checkout failures

## Timeline (UTC)
- 14:02 Deploy of payments-gateway v4.18.0 begins (routine dependency bump).
- 14:09 Error rate on POST /checkout rises from 0.2% to 31%.
- 14:11 Pager fires for checkout-availability SLO. Priya (on-call) acknowledges.
- 14:14 Priya notices the errors are all `TokenExpired` from the card vault client.
- 14:19 Suspected cause: v4.18.0 upgraded the vault SDK, which changed the default token TTL from 15 minutes to 15 seconds. Tokens expire mid-request under load.
- 14:22 Rollback to v4.17.3 started.
- 14:31 Rollback complete. Error rate back to 0.3%.
- 14:45 Confirmed no stuck orders; 212 payments that failed were not charged.

## Impact
- About 4,100 checkout attempts failed over 22 minutes (14:09–14:31).
- No customer was double charged. No data loss.
- Mobile and web both affected. Subscriptions renewals were not affected (different path).

## Current status
- Stable on v4.17.3. v4.18.0 is blocked from redeploy.
- Fix in review: pin token TTL explicitly (PR #8841). Earliest redeploy tomorrow after canary.

## Open questions
- Why did canary not catch it? Canary traffic is too low to hit token reuse.
- Should SDK upgrades require a load test? To discuss Thursday.
