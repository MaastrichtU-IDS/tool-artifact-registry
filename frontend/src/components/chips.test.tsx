import { render, screen } from '@testing-library/react'
import { MemoryRouter } from 'react-router-dom'
import { describe, expect, it } from 'vitest'
import { ArtifactTypeChip, AvailabilityBadge, HealthDot, LicenseChip, OriginChip, RunStatus } from './chips'

const wrap = (ui: React.ReactNode) => render(<MemoryRouter>{ui}</MemoryRouter>)

describe('OriginChip', () => {
  it('renders local records plainly', () => {
    wrap(<OriginChip origin={{ kind: 'local' }} />)
    expect(screen.getByText('local')).toBeInTheDocument()
  })

  it('names the peer and says how stale the cache is', () => {
    wrap(
      <OriginChip
        origin={{
          kind: 'peer',
          peer_title: 'MUMC',
          peer_base_iri: 'https://reg.mumc.nl',
          cached_at: new Date(Date.now() - 3 * 3600 * 1000).toISOString(),
        }}
      />,
    )
    expect(screen.getByText(/peer: MUMC/)).toBeInTheDocument()
    expect(screen.getByText(/cached 3h ago/)).toBeInTheDocument()
  })

  it('says so when a cross-link has not been resolved', () => {
    wrap(<OriginChip origin={{ kind: 'peer' }} />)
    expect(screen.getByText('not resolved yet')).toBeInTheDocument()
  })
})

describe('status indicators', () => {
  it('never conveys health by colour alone', () => {
    wrap(<HealthDot health="down" />)
    expect(screen.getByText('down')).toBeInTheDocument()
  })

  it('never conveys run status by colour alone', () => {
    wrap(<RunStatus status="failed" />)
    expect(screen.getByText('failed')).toBeInTheDocument()
  })
})

describe('LicenseChip', () => {
  it('shortens an SPDX IRI to its identifier', () => {
    wrap(<LicenseChip license="https://spdx.org/licenses/Apache-2.0" />)
    expect(screen.getByText('Apache-2.0')).toBeInTheDocument()
  })

  it('distinguishes absent from unlicensed', () => {
    wrap(<LicenseChip />)
    expect(screen.getByText('licence not stated')).toBeInTheDocument()
  })
})

describe('AvailabilityBadge', () => {
  it('explains what metadata-only means', () => {
    wrap(<AvailabilityBadge availability="metadata-only" />)
    expect(screen.getByTitle(/not retrievable/i)).toBeInTheDocument()
  })
})

describe('ArtifactTypeChip', () => {
  it('falls back to the last IRI segment when the type has no label', () => {
    wrap(<ArtifactTypeChip type={{ iri: 'http://edamontology.org/data_2048', source: 'bundled' }} />)
    expect(screen.getByText('data_2048')).toBeInTheDocument()
  })

  it('links to artifacts of that type', () => {
    wrap(<ArtifactTypeChip type={{ iri: 'http://edamontology.org/data_2048', label: 'Report', source: 'bundled' }} />)
    expect(screen.getByRole('link', { name: /Report/ })).toHaveAttribute(
      'href',
      '/artifacts?conforms_to=http%3A%2F%2Fedamontology.org%2Fdata_2048',
    )
  })
})
