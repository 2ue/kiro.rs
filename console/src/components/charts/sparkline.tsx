import { Area, AreaChart, ResponsiveContainer } from 'recharts'

export default function Sparkline({ values, color, id }: { values: number[]; color: string; id: string }) {
  const gid = `spark-${id.replace(/\W/g, '')}`
  return (
    <ResponsiveContainer width="100%" height="100%">
      <AreaChart data={values.map((v, i) => ({ i, v }))} margin={{ top: 2, bottom: 0, left: 0, right: 0 }}>
        <defs>
          <linearGradient id={gid} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={color} stopOpacity={0.35} />
            <stop offset="100%" stopColor={color} stopOpacity={0} />
          </linearGradient>
        </defs>
        <Area type="monotone" dataKey="v" stroke={color} strokeWidth={1.5} fill={`url(#${gid})`} isAnimationActive={false} />
      </AreaChart>
    </ResponsiveContainer>
  )
}
