export async function resolve(specifier, context, nextResolve) {
	try {
		return await nextResolve(specifier, context)
	} catch (err) {
		if (err && err.code === 'ERR_MODULE_NOT_FOUND' && specifier.startsWith('.') && !/\.[cm]?[jt]s$/.test(specifier)) {
			return nextResolve(specifier + '.ts', context)
		}
		throw err
	}
}
