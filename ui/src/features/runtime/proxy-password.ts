// 代理密码的提交规则。No runtime imports so it can be tested with `node --test`.

/** 输入框的操作：保持不变（默认）、清除（已经过危险确认）。输入新值时以输入框内容为准。 */
export type ProxyPasswordAction = 'keep' | 'clear'

/**
 * 读取到的代理密码是否表示“已设置”：后端对已设置的密码固定回 `********`、未设置回 null；
 * 老后端回的是 `ab...yz` 形式的脱敏值，同样只表示“已设置”。两者都不是明文，不能原样回传当密码。
 */
export function proxyPasswordIsSet(value: string | null | undefined): boolean {
  return typeof value === 'string' && value.length > 0
}

/**
 * 保存时 `proxyPassword` 字段怎么带：
 * - 选了清除：带 null，后端清空密码；
 * - 输入框有内容：带新密码；
 * - 其它（没动过、或输入几个字又删空）：不带这个字段，后端保持原密码。
 * 输入框为空绝不能变成 null，否则会把线上密码删掉。
 */
export function proxyPasswordPatch(input: string, action: ProxyPasswordAction): { proxyPassword?: string | null } {
  if (action === 'clear') return { proxyPassword: null }
  return input.length > 0 ? { proxyPassword: input } : {}
}
