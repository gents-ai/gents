/* PROTOTYPE ONLY: chosen behaviour hues, provided once from the shell so
   every avatar reads them without threading a prop through each screen. */
import { createContext, useContext } from 'react'

export const BehaviorColorsContext = createContext<Record<string, number>>({})
export const useBehaviorColors = () => useContext(BehaviorColorsContext)
