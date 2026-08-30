import { Link, useLocation } from 'react-router-dom'
import { EmptyState } from '../components/common'

export default function NotFound() {
  const { pathname } = useLocation()
  return (
    <EmptyState
      title="No such record here"
      body={
        <>
          Nothing is registered at <code>{pathname}</code>. If this is an IRI from another
          registry, open it there — every registry is authoritative for the IRIs it minted.
        </>
      }
      action={<Link className="chip accent" to="/software">Back to software</Link>}
    />
  )
}
