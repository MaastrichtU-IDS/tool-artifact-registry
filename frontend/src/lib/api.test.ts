import { describe, expect, it } from 'vitest'
import { ApiError, qs } from './api'

describe('qs', () => {
  it('drops empty and false values so the URL stays readable', () => {
    expect(qs({ q: 'shacl', license: undefined, federated: false, limit: 25 })).toBe('?q=shacl&limit=25')
  })
  it('returns nothing when every parameter is empty', () => {
    expect(qs({ q: '', cursor: undefined })).toBe('')
  })
})

describe('ApiError.fieldErrors', () => {
  it('maps a SHACL validation report back onto form fields', () => {
    const report = `
[] a sh:ValidationReport ;
    sh:conforms false ;
    sh:result [
        a sh:ValidationResult ;
        sh:resultSeverity sh:Violation ;
        sh:focusNode "urn:new" ;
        sh:resultPath <https://schema.org/name> ;
        sh:sourceConstraintComponent sh:MinCountConstraintComponent ;
        tar:jsonField "name" ;
        sh:resultMessage "Software needs a name"
    ] ;
    sh:result [
        a sh:ValidationResult ;
        sh:resultSeverity sh:Violation ;
        sh:focusNode "urn:new" ;
        sh:resultPath <https://w3id.org/tar/ns#kind> ;
        sh:sourceConstraintComponent sh:InConstraintComponent ;
        tar:jsonField "kind" ;
        sh:resultMessage "kind must be one of service, library, cli, workflow"
    ] .`
    const err = new ApiError(
      { type: 'x', title: 'Write rejected by SHACL validation', status: 422, report },
      422,
    )
    const fields = err.fieldErrors()
    expect(fields.name).toBe('Software needs a name')
    expect(fields.kind).toMatch(/must be one of service/)
  })

  it('returns nothing when the problem carries no report', () => {
    const err = new ApiError({ type: 'x', title: 'Forbidden', status: 403 }, 403)
    expect(err.fieldErrors()).toEqual({})
  })
})
