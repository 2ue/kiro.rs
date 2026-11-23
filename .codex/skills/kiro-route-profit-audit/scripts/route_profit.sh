#!/usr/bin/env bash
# Per-route profit audit for a claude-pool instance fronted by sub2api. Read-only.
#
# usage: route_profit.sh <ssh-alias> <pool-alias> <sub2api-pg-container> [cost-ratio] [window-sql]
#   ssh-alias            e.g. us-debian-17
#   pool-alias           e.g. claude-pool-1   (containers <alias>-postgres, base_url http://<alias>:8990)
#   sub2api-pg-container e.g. sub2api-b-postgres
#   cost-ratio           supplier price as fraction of official list price, default 0.3
#   window-sql           SQL predicate on created_at, default = today in Asia/Shanghai
#
# Attribution model (see ../references/cost-model.md):
#   * cost and list price per route come from the claude-pool side (immutable endpoint per request)
#   * user revenue per route = est(route) x user_share(route) x m_user
#       m_user = sum(actual)/sum(total) over non-admin sub2api traffic for the pool  (observed multiplier)
#       user_share(route) = 1 - admin list-price share on that route (sub2api, by current base_url)
#   * sub2api per-account base_url is mutable; routes whose sub2api list price disagrees with c1 est
#     by >15% are flagged "attrib?" (account renamed mid-window). Totals are unaffected.
# Output: reconcile line, totals (users / admin / internal), per-route table sorted by margin, recommendation.
# On macOS pipe through `LC_ALL=C cut -c1-220` if the terminal complains about bytes.
set -euo pipefail
HOST="${1:?ssh alias}"; POOL="${2:?pool alias}"; S2PG="${3:?sub2api pg container}"
RATIO="${4:-0.3}"
WIN="${5:-created_at >= (date_trunc('day', now() at time zone 'Asia/Shanghai') at time zone 'Asia/Shanghai')}"

remote() { ssh -o BatchMode=yes "$HOST" "$@"; }
C() { remote "docker exec ${POOL}-postgres psql -X -qAt -U app -d app -c $(printf %q "$1")"; }
B() { remote "docker exec ${S2PG} psql -X -qAt -U sub2api -d sub2api -c $(printf %q "$1")"; }

echo "capture: $(remote "TZ=Asia/Shanghai date '+%Y-%m-%d %H:%M %Z'")  pool=$POOL  sub2api=$S2PG  cost_ratio=$RATIO"
echo "window:  $WIN"
echo

C "SELECT regexp_replace(endpoint,'^/dfcache/|/v1/messages$','','g')||'|'||count(*)||'|'||
     round(sum(original_cost_usd)::numeric,4)||'|'||round(sum(estimated_cost_usd)::numeric,4)||'|'||
     coalesce(round((100.0*sum((data->'accountBilling'->'reportedUsage'->>'cacheReadInputTokens')::numeric)
       /nullif(sum((data->'accountBilling'->'reportedUsage'->>'totalInputTokens')::numeric),0))::numeric,1),0)
   FROM usage_records WHERE deleted_at IS NULL AND status='success' AND $WIN GROUP BY endpoint" > /tmp/_c1.txt

B "SELECT regexp_replace(a.credentials->>'base_url','^.*/dfcache/','')||'|'||coalesce(us.role::text,'user')||'|'||count(*)||'|'||
     round(sum(u.total_cost)::numeric,4)||'|'||round(sum(u.actual_cost)::numeric,4)
   FROM usage_logs u JOIN accounts a ON a.id=u.account_id LEFT JOIN users us ON us.id=u.user_id
   WHERE a.credentials->>'base_url' ~ ('^http://${POOL}:8990') AND u.$WIN
   GROUP BY a.credentials->>'base_url', us.role" > /tmp/_s2.txt

python3 - "$RATIO" <<'PY'
import sys
R=float(sys.argv[1])
c1={}
for l in open('/tmp/_c1.txt'):
    e,n,o,s,h=l.strip().split('|'); c1[e]=dict(n=int(n),orig=float(o),est=float(s),hit=float(h))
s2={}
for l in open('/tmp/_s2.txt'):
    e,role,n,tc,ac=l.strip().split('|'); e=e or '<root>'
    d=s2.setdefault(e,{'admin':[0,0.0,0.0],'user':[0,0.0,0.0]}); k='admin' if role=='admin' else 'user'
    d[k][0]+=int(n); d[k][1]+=float(tc); d[k][2]+=float(ac)

tot_est=sum(v['est'] for v in c1.values())
tot_tc=sum(d['admin'][1]+d['user'][1] for d in s2.values())
u_tc=sum(d['user'][1] for d in s2.values()); u_ac=sum(d['user'][2] for d in s2.values())
a_tc=sum(d['admin'][1] for d in s2.values()); a_ac=sum(d['admin'][2] for d in s2.values())
m_user=(u_ac/u_tc) if u_tc else 0.0
print(f"reconcile: c1 est={tot_est:.2f}  sub2api total_cost={tot_tc:.2f}  diff={100*(tot_tc-tot_est)/tot_est if tot_est else 0:+.1f}%   observed user multiplier m={m_user:.4f}")
print()

rows=[]; T=dict(orig=0.0,est=0.0,user_rev=0.0,user_cost=0.0,adm_cost=0.0,int_cost=0.0)
for e,v in c1.items():
    s=s2.get(e,{'admin':[0,0.0,0.0],'user':[0,0.0,0.0]})
    s_tc=s['admin'][1]+s['user'][1]
    cost=v['orig']*R; gain=(v['est']-v['orig'])/v['orig']*100 if v['orig'] else 0.0
    T['orig']+=v['orig']; T['est']+=v['est']
    if s_tc==0:                                   # no sub2api revenue at all -> internal / direct
        T['int_cost']+=cost
        rows.append(dict(e=e,n=v['n'],gain=gain,cost=cost,rev=0.0,profit=-cost,margin=None,hit=v['hit'],kind='int',flag=''))
        continue
    adm_share=s['admin'][1]/s_tc
    user_cost=cost*(1-adm_share); adm_cost=cost*adm_share
    rev=v['est']*(1-adm_share)*m_user             # c1 list price x user share x observed multiplier
    profit=rev-user_cost
    flag='attrib?' if v['est']>0 and abs(s_tc-v['est'])/v['est']>0.15 else ''
    if rev<=0:                                     # admin-only route in this window: no user revenue to grade
        T['adm_cost']+=cost
        rows.append(dict(e=e,n=v['n'],gain=gain,cost=cost,rev=0.0,profit=-cost,margin=None,hit=v['hit'],kind='admin',flag=flag)); continue
    T['user_rev']+=rev; T['user_cost']+=user_cost; T['adm_cost']+=adm_cost
    rows.append(dict(e=e,n=v['n'],gain=gain,cost=user_cost,rev=rev,profit=profit,margin=profit/rev*100,hit=v['hit'],kind='user',flag=flag))

print(f"TOTAL    orig={T['orig']:.2f}  supplier_cost={T['orig']*R:.2f}  list(est)={T['est']:.2f}")
print(f"USERS    revenue={T['user_rev']:.2f}  cost={T['user_cost']:.2f}  profit={T['user_rev']-T['user_cost']:+.2f}  margin={(100*(T['user_rev']-T['user_cost'])/T['user_rev']) if T['user_rev'] else 0:.1f}%   (sub2api actual_cost users={u_ac:.2f})")
print(f"ADMIN    revenue={a_ac:.2f}  cost={T['adm_cost']:.2f}  net={a_ac-T['adm_cost']:+.2f}")
print(f"INTERNAL cost={T['int_cost']:.2f}  (routes with no sub2api revenue; 'admin' rows = only admin traffic in window)")
print()
print(f"{'route':28} {'n':>6} {'差额%':>7} {'cost':>8} {'revenue':>8} {'profit':>8} {'margin%':>8} {'hit%':>6}  note")
for r in sorted(rows,key=lambda r:(r['kind']!='user', -(r['margin'] if r['margin'] is not None else -1e9))):
    m={'int':'  int.','admin':' admin'}.get(r['kind']) or f"{r['margin']:7.1f}"
    print(f"{r['e']:28} {r['n']:6d} {r['gain']:7.1f} {r['cost']:8.2f} {r['rev']:8.2f} {r['profit']:+8.2f} {m:>8} {r['hit']:6.1f}  {r['flag']}")
print()
bad=[r for r in rows if r['kind']=='user' and r['margin'] is not None and r['margin']<0]
weak=[r for r in rows if r['kind']=='user' and r['margin'] is not None and 0<=r['margin']<5]
ok=[r for r in rows if r['kind']=='user' and r['margin'] is not None and r['margin']>=5]
print("RECOMMEND  disable (negative margin):", ', '.join(r['e'] for r in bad) or '-')
print("           review  (margin < 5%):   ", ', '.join(r['e'] for r in weak) or '-')
print("           keep:                     ", ', '.join(r['e'] for r in sorted(ok,key=lambda r:-r['margin'])) or '-')
print(f"           break-even 差额 at m={m_user:.2f}: {((R/m_user)-1)*100 if m_user else 0:.0f}%   | 'attrib?' = sub2api account renamed mid-window; totals unaffected   | nothing was modified")
PY
rm -f /tmp/_c1.txt /tmp/_s2.txt
